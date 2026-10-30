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
