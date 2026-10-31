"""Export the AFx-Rep style encoder (ST-ITO, Steinmetz et al., ISMIR 2024) to ONNX.

    python -I export_afx_rep.py <afx-rep.ckpt> <out.onnx> <demo.wav> <fixture_out.bin>

The checkpoint is read with weights_only=True and the Cnn14 below is a clean definition, so no
code from the st-ito repository runs. The graph's input is a power spectrogram: the STFT moves to
src/audio/ml_style.rs, while the mel filterbank, log scaling and normalisation stay in the graph
with the checkpoint's own values.

Input   power: [batch * 2, 1, frames, 1025] (mid, side, mid, side, ...)
Output  mid, side: [batch, 512], not normalised
"""
import sys
import time
import wave

import numpy as np
import onnx
import onnxruntime as ort
import torch
import torch.nn as nn
import torch.nn.functional as F

N_FFT, HOP, BINS, MELS, DIM = 2048, 1024, 1025, 128, 512
SR = 48000
AMIN = 1e-10


class ConvBlock(nn.Module):
    def __init__(self, ch_in, ch_out):
        super().__init__()
        self.conv1 = nn.Conv2d(ch_in, ch_out, 3, padding=1, bias=False)
        self.conv2 = nn.Conv2d(ch_out, ch_out, 3, padding=1, bias=False)
        self.bn1 = nn.BatchNorm2d(ch_out)
        self.bn2 = nn.BatchNorm2d(ch_out)

    def forward(self, x, pool):
        x = F.relu(self.bn1(self.conv1(x)))
        x = F.relu(self.bn2(self.conv2(x)))
        return F.avg_pool2d(x, pool) if pool > 1 else x


class StyleEncoder(nn.Module):
    def __init__(self, mel_w):
        super().__init__()
        self.register_buffer("mel_w", mel_w)
        self.conv_block1 = ConvBlock(1, 64)
        self.conv_block2 = ConvBlock(64, 128)
        self.conv_block3 = ConvBlock(128, 256)
        self.conv_block4 = ConvBlock(256, 512)
        self.conv_block5 = ConvBlock(512, 1024)
        self.conv_block6 = ConvBlock(1024, 2048)
        self.fc_mid = nn.Linear(2048, DIM)
        self.fc_side = nn.Linear(2048, DIM)

    def forward(self, power):
        mel = torch.matmul(power, self.mel_w)
        x = 10.0 * torch.log10(torch.clamp(mel, min=AMIN))
        x = (torch.clamp(x, -80.0, 40.0) + 80.0) / 120.0 * 2.0 - 1.0
        for block in (self.conv_block1, self.conv_block2, self.conv_block3, self.conv_block4, self.conv_block5):
            x = block(x, 2)
        x = self.conv_block6(x, 1)
        x = torch.mean(x, dim=3)
        x = torch.amax(x, dim=2) + torch.mean(x, dim=2)
        return self.fc_mid(x[0::2]), self.fc_side(x[1::2])


def read_wav(path):
    with wave.open(path, "rb") as w:
        sr, ch, width, n = w.getframerate(), w.getnchannels(), w.getsampwidth(), w.getnframes()
        raw = w.readframes(n)
    assert width == 3, f"expected 24-bit demo track, got {width * 8}"
    b = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
    v = b[:, 0] | (b[:, 1] << 8) | (b[:, 2] << 16)
    x = (np.where(v >= 1 << 23, v - (1 << 24), v) / float(1 << 23)).astype(np.float32)
    return x.reshape(-1, ch).T, sr


def to_48k(x, sr):
    if sr == SR:
        return x
    n = int(x.shape[1] * SR / sr)
    t = torch.from_numpy(x)[None]
    return F.interpolate(t, size=n, mode="linear", align_corners=False)[0].numpy()


def ref_power(x, conv_real, conv_imag):
    """torchlibrosa's STFT, using the kernels stored in the checkpoint. x: [n, samples]"""
    pad = N_FFT // 2
    y = F.pad(x[:, None], (pad, pad), mode="reflect")
    re = F.conv1d(y, conv_real, stride=HOP)
    im = F.conv1d(y, conv_imag, stride=HOP)
    return (re**2 + im**2).transpose(1, 2)[:, None]  # [n, 1, frames, bins]


def peak_ms(clip):
    """clip: [2, samples] -> [2, samples] mid and side after the peak normalisation of get_param_embeds"""
    clip = clip / clip.abs().max().clamp(min=1e-8)
    return torch.stack([(clip[0] + clip[1]) / 2, (clip[0] - clip[1]) / 2])


def main():
    ckpt_path, out_path, demo_path, fixture_path = sys.argv[1:5]
    ckpt = torch.load(ckpt_path, map_location="cpu", weights_only=True)
    state = {k[len("encoder."):]: v for k, v in ckpt["state_dict"].items() if k.startswith("encoder.")}
    print("non-weight keys:", [k for k in ckpt if k != "state_dict"][:8])
    mel_w = state["logmel_extractor.melW"].float()
    conv_real = state["spectrogram_extractor.stft.conv_real.weight"].float()
    conv_imag = state["spectrogram_extractor.stft.conv_imag.weight"].float()
    print("melW", tuple(mel_w.shape), "conv_real", tuple(conv_real.shape))

    model = StyleEncoder(mel_w)
    loaded = {k: v for k, v in state.items() if k.startswith(("conv_block", "fc_"))}
    missing, unexpected = model.load_state_dict(loaded, strict=False)
    assert missing == ["mel_w"] and not unexpected, (missing, unexpected)
    model.eval()
    print("params: %.1f M" % (sum(p.numel() for p in model.parameters()) / 1e6))

    # The Rust STFT is the textbook definition: check it against the checkpoint's kernels
    window = torch.hann_window(N_FFT, periodic=True)
    probe = torch.randn(2, SR)
    a = ref_power(probe, conv_real, conv_imag)
    b = torch.stft(probe, N_FFT, HOP, window=window, center=True, pad_mode="reflect", return_complex=True)
    b = (b.abs() ** 2).transpose(1, 2)[:, None]
    print("stft kernel vs periodic hann: max rel err %.2e" % float((a - b).abs().max() / a.max()))

    audio, sr = read_wav(demo_path)
    audio = to_48k(audio, sr)
    print("demo", audio.shape, sr)
    clips = [torch.from_numpy(audio[:, s:s + 4 * SR].copy()) for s in np.linspace(0, audio.shape[1] - 4 * SR - 1, 10).astype(int)]

    dummy = torch.zeros(2, 1, 189, BINS)
    torch.onnx.export(
        model, (dummy,), out_path, input_names=["power"], output_names=["mid", "side"], opset_version=17,
        dynamic_axes={"power": {0: "items", 2: "frames"}, "mid": {0: "batch"}, "side": {0: "batch"}},
        dynamo=False,
    )
    onnx.checker.check_model(out_path, full_check=False)
    sess_opts = ort.SessionOptions()
    sess = ort.InferenceSession(out_path, sess_opts, providers=["CPUExecutionProvider"])

    worst, cpu = 1.0, []
    for clip in clips:
        ms = peak_ms(clip)
        power = ref_power(ms, conv_real, conv_imag)
        with torch.no_grad():
            pm, ps = model(power)
        t = time.perf_counter()
        om, os_ = sess.run(None, {"power": power.numpy()})
        cpu.append(time.perf_counter() - t)
        for p, o in ((pm, om), (ps, os_)):
            worst = min(worst, float(F.cosine_similarity(p, torch.from_numpy(o), dim=-1)))
    print("parity worst cosine %.6f on %d clips" % (worst, len(clips)))
    print("ort time per 4 s clip: %.3f s (median)" % float(np.median(cpu)))

    long_clip = peak_ms(torch.from_numpy(np.tile(audio, (1, 2))[:, :10 * SR].copy()))
    t = time.perf_counter()
    sess.run(None, {"power": ref_power(long_clip, conv_real, conv_imag).numpy()})
    print("ort time per 10 s clip: %.3f s" % (time.perf_counter() - t))

    # Two identical-shape clips in one batch must give the same answers as one at a time
    batch = torch.cat([ref_power(peak_ms(c), conv_real, conv_imag) for c in clips[:2]])
    t = time.perf_counter()
    bm, _ = sess.run(None, {"power": batch.numpy()})
    print("ort time for a batch of 2 clips: %.3f s" % (time.perf_counter() - t))
    single, _ = sess.run(None, {"power": batch[:2].numpy()})
    print("batch vs single cosine %.6f" % float(F.cosine_similarity(torch.from_numpy(bm[:1]), torch.from_numpy(single), dim=-1)))

    # Fixture for the Rust parity test: the first 4 s at 48 kHz and PyTorch's embedding of it
    clip = clips[3]
    with torch.no_grad():
        m, s = model(ref_power(peak_ms(clip), conv_real, conv_imag))
    with open(fixture_path, "wb") as f:
        f.write(np.array([clip.shape[1]], dtype="<u4").tobytes())
        f.write(clip.numpy().astype("<f4").tobytes())
        f.write(m.numpy().astype("<f4").tobytes())
        f.write(s.numpy().astype("<f4").tobytes())


if __name__ == "__main__":
    main()
