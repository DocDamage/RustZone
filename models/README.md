# Bundled models

## htdemucs.onnx

Stem-separation model used for "Separate stems (AI)" (drums, bass, other, vocals). Stored with
Git LFS — run `git lfs install` before cloning, or `git lfs pull` afterwards.

- **Model:** HTDemucs (Hybrid Transformer Demucs v4) by Meta AI —
  Rouard, Massa & Défossez, *Hybrid Transformers for Music Source Separation*, ICASSP 2023.
  <https://github.com/facebookresearch/demucs>
- **ONNX conversion:** <https://huggingface.co/Ghilda/htdemucs-onnx> (MIT), STFT/iSTFT
  externalised; this app performs them in `src/audio/ml_separator.rs`.
- **SHA-256:** `e528a932a7d091e15938369135569884b62c2193fb11044c3d4a0d4c7b9221af`

### Licence (Demucs)

```
MIT License

Copyright (c) Meta Platforms, Inc. and affiliates.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## afx_rep.onnx

Production-style model used for "Match reference (AI)". It scores how close two clips sound
(EQ, dynamics, width, saturation) and never processes audio. Stored with Git LFS.

- **Model:** AFx-Rep, a Cnn14 encoder from ST-ITO by Steinmetz et al. —
  *ST-ITO: Controlling Audio Effects for Style Transfer with Inference-Time Optimization*,
  ISMIR 2024. <https://github.com/csteinmetz1/st-ito>
- **Weights:** `afx-rep.ckpt` from <https://huggingface.co/csteinmetz1/afx-rep>
  (revision `6582cb5414094146759319819a4242967ecc7e39`), published under Apache-2.0.
  The Cnn14 architecture derives from PANNs (MIT).
- **ONNX conversion:** `export_afx_rep.py` in this folder (opset 17, 77.6 M parameters, 48 kHz).
  The STFT is externalised and performed in `src/audio/ml_style.rs`; the checkpoint's mel
  filterbank stays in the graph. Input `power` is `[batch * 2, 1, frames, 1025]` (mid, side, ...);
  outputs `mid` and `side` are `[batch, 512]`. This is a modified form of the original weights.
- **Parity:** cosine 1.000000 against PyTorch on 10 clips of the demo track. Roughly 0.1 s per
  4 s clip on a 20-thread CPU.
- **SHA-256:** `761a5d7e6bee5e67df2ea474b57c1bd7cf46e376aa3460ad4e84322452711fec`

To reproduce it, with the checkpoint downloaded (it's read with `weights_only=True`, so nothing
from the st-ito repository runs):

```bash
python -I models/export_afx_rep.py afx-rep.ckpt models/afx_rep.onnx demo_mastering_track.wav fixture.bin
```

The last 4096 bytes of `fixture.bin` are `tests/fixtures/afx_rep_demo_embedding.bin`.

### Licence (AFx-Rep)

Apache License 2.0: <https://www.apache.org/licenses/LICENSE-2.0>
