# RustZone: AI Reference Matching Plan

Oct 30, 2026 · @Doc

## Goal and approach

Add a **Match reference (AI)** button that sets the existing 19-module suite so the track sounds like a chosen reference. The model never processes audio itself; it only scores how close two clips sound, and RustZone's own DSP does all the processing.

- **Style model:** AFx-Rep from ST-ITO (Steinmetz et al., ISMIR 2024, code Apache 2.0). It turns a clip into a 512-dim mid and a 512-dim side embedding that track production style (EQ, dynamics, width, saturation).
- **Search:** CMA-ES over normalised module parameters. Each candidate renders a short excerpt through a cloned `MasteringSuite`, gets embedded, and is scored by distance to the reference embedding.
- **Result:** the best parameters are applied in one `edit_suite` call, so they're a single undo step and land in the current snapshot slot.
- **Why this over an audio LLM:** small audio LLMs are speech-focused and can't give reliable mix judgements. DeepAFx-ST and Diff-MST are non-commercial licences, and FxNorm-Automix outputs audio, not settings.

It sits next to two features that already exist: Match EQ's *Learn from reference track* (tone only) and the rule-based Master Assistant (loudness and width only). Neither changes; this is a third, broader option.

## Phase 0: Feasibility spike in Python (1–2 days)

This phase checks the three things that could kill the plan before you write any Rust. Do it in a throwaway folder outside the repo.

1. **Weights licence.** Clone `csteinmetz1/st-ito`, find where `load_param_model()` downloads its checkpoint, and confirm the weights carry a licence that allows commercial redistribution. The code is Apache 2.0, but the weights may not be. If they're restricted, stop and see Risks.
2. **Reproduce the result.** Run `scripts/run_optim.py` on `demo_mastering_track.wav` against a reference master. Listen to the before/after, so you know what quality to expect before you port anything.
3. **Export to ONNX.** Wrap the model in a module that takes raw stereo audio (`[1, 2, N]` at a fixed rate) and returns `mid` and `side` embeddings. Then call `torch.onnx.export` with opset 17 and a dynamic length axis. If the model does its own STFT or mel transform, move that into Rust, the same way `ml_separator.rs` handles STFT for HTDemucs.
4. **Parity check.** Run the ONNX file with `onnxruntime` in Python and compare its embeddings with PyTorch's on 10 clips. The target is cosine similarity above 0.999.
5. **Measure.** Write down the model's file size and CPU time per 10 s clip. That number sets the iteration budget in Phase 3.

**Exit criteria:** a licence you can ship, `models/afx_rep.onnx` passing the parity check, and a per-clip CPU time on your machine.

## Phase 1: Load the style model in Rust (1 day)

This phase copies the pattern `src/audio/ml_separator.rs` already uses, with the same `ort` 2.0.0-rc.10 dependency. No new crates are needed.

- **New file `src/audio/ml_style.rs`** containing a `StyleModel { session: Session }` with `load(path)`, `load_default()` and a `find_model()` that looks for `afx_rep.onnx` the same way the HTDemucs loader does.
- **`embed(&mut self, l: &[f32], r: &[f32], sr: u32) -> StyleEmbedding`**, which resamples to the model's rate with the existing `audio::resample`, runs the session and returns `{ mid: [f32; 512], side: [f32; 512] }`.
- **`StyleEmbedding::distance(&self, other) -> f32`**, computed as `(1 - cos(mid)) + w_side * (1 - cos(side))` with `w_side = 0.5` to start. Keep the weight a constant you can tune.
- **Model file.** Add `models/afx_rep.onnx` via Git LFS (`.gitattributes` already tracks the models folder). Add a section to `models/README.md` with its origin, licence and SHA-256, matching the HTDemucs entry.
- **Threading.** Build the session with `GraphOptimizationLevel::Level3` and an intra-op thread count of physical cores minus one, so the UI stays responsive.

## Phase 2: Define the searchable parameter space (1–2 days)

The optimiser works on a flat vector in \[0, 1\]. `ParamInfo` already holds `min`, `max`, `log` and the unit for every parameter, so mapping in both directions is generic.

- **New file `src/dsp/style_space.rs`** with `struct SearchParam { kind: ModuleKind, index: usize, info: ParamInfo }` and `struct SearchSpace { params: Vec<SearchParam> }`.
- **`encode(&MasteringSuite) -> Vec<f32>`** and **`decode(&[f32], &mut MasteringSuite)`**. Use a log mapping when `info.log` is set (Hz values), linear otherwise, then `ParamInfo::stepped` to snap to the slider grid.
- **Leave out** `Toggle` and `Choice` params (CMA-ES needs continuous values), enable/disable flags, oversampling and chain order. The user's choices for those stay as they are.
- **Start small, 12–20 dimensions:** Dynamic EQ (low shelf, two bells, high shelf: gain and frequency), Vintage Compressor (threshold, ratio, mix), Stereo Imager (width), Harmonic Exciter (drive, mix), and Maximizer threshold. Widen the set only once this converges reliably.
- **Hold loudness out of the search.** Fix the maximizer ceiling at the user's loudness target, and loudness-match each candidate before embedding it. Otherwise the search just learns "louder", which is the same trap your loudness-matched A/B already avoids.
- **Only enabled modules count.** Build the space from the modules ticked in the chain, so the user decides what the AI may touch.

## Phase 3: CMA-ES optimiser and render loop (3–4 days)

This is the core of the feature: `src/dsp/style_match.rs` exposing `match_to_reference(track, reference, suite, model, progress) -> Result<MatchResult, String>`.

1. **Pick excerpts.** Take three 4 s windows from the track: the loudest, the median and one near the start of the loudest section, using momentary LUFS from the existing meter. Embed the whole reference once, as 4 s windows averaged together.
2. **Evaluate a candidate.** Clone the suite (`MasteringSuite` derives `Clone`; take the start point from `engine.dsp_suite_snapshot()`), `decode` the vector into it, `reset()`, run `process_stereo` on each excerpt with `latency_samples()` of pre-roll, and throw the pre-roll away. Then loudness-match the output to the reference, embed it, and average the distances.
3. **Optimiser.** Use CMA-ES with population `4 + 3·ln(n)` (about 12 when n = 16), starting at the user's current settings with step size 0.2. Either check the licence of a crate such as `cmaes` or write a separable CMA-ES yourself; that's roughly 200 lines and avoids the dependency.
4. **Budget.** Stop after 30 generations (about 360 evaluations), after a wall-clock limit (90 s by default), or when the best score improves by less than 0.1% over 5 generations. Run candidates in parallel with `std::thread::scope`, one suite clone per thread.
5. **Regularise.** Add a small penalty for moving away from the starting settings (`0.05 · |x - x0|²`), so it doesn't make big changes for tiny gains.
6. **Return** the best vector, its score, the starting score and a per-parameter diff, so the UI can say what changed.

With the fixed per-candidate cost (three excerpts plus three embeddings), this will mostly be CPU-bound in the spectral modules. Profile it with `examples/bench_modules.rs` before tuning anything.

## Phase 4: UI, undo and background job (2 days)

This phase reuses the background-job pattern from **Separate stems (AI)**, so the feature behaves the same way users already know.

- **Entry point.** Add a **Match reference (AI)** button in the referencing area of `src/ui/app.rs`. It's enabled only when a track and a reference are both loaded and `afx_rep.onnx` was found.
- **Background thread.** Spawn the job with a progress callback, as the stem separation does. The status line shows the generation and best score (for example “Matching reference: gen 12/30, 41% closer”), and a **Cancel** button sets an `AtomicBool` the loop checks once per generation.
- **Apply as one undo step.** When it finishes, call `self.edit_suite("style_match", move |s| space.decode(&best, s))`. Ctrl+Z then reverts the whole match, and it lands in the current snapshot slot. Suggest A/B-ing it against another slot.
- **Report.** Show a small panel listing each changed parameter (old to new, using `ParamInfo::format`), plus the improvement percentage. That keeps the result explainable rather than a black box.
- **Sessions.** Nothing new to save; the result is ordinary suite state and already goes into `.mstsession` files and presets.
- **Scope toggle.** Add a checkbox: “Only adjust enabled modules” (default on).

## Phase 5: Tests, benchmarks and CI (1–2 days)

The parts that don't need the model are tested on every CI run. Model-dependent tests skip themselves, as `tests/ml_separation_test.rs` already does, because CI checks out with `lfs: false`.

- **`tests/style_space_test.rs`** (no model): encode then decode round-trips every searchable parameter to within one slider step, and log mapping keeps Hz values inside `min`/`max`.
- **`tests/style_match_test.rs`** (no model): optimise against a mock distance (squared distance to a known vector) and check that CMA-ES converges in under 30 generations. This tests the optimiser on its own.
- **Round-trip test** (model, skipped in CI): process `demo_mastering_track.wav` with known settings to make a fake reference, start from defaults, and check that the match gets at least 50% closer in embedding distance. Also check that the known EQ gains are recovered to within 2 dB.
- **Determinism:** seed the CMA-ES random number generator so a given track, reference and seed always produce the same result. That makes bug reports reproducible.
- **Benchmark:** add a `style_eval` case to `examples/bench_modules.rs` that times one candidate evaluation, so CI catches slowdowns.
- **Listening tests:** run 10 track/reference pairs across genres and keep the notes in `docs/`. Numbers don't replace ears for this feature.

## Phase 6 (optional): Text-prompt layer

Only start this once Phases 0–5 ship. It lets a user type “warmer, less harsh, wider” and get a starting point that the reference match (or the user) then refines.

- **Approach:** LLM2Fx (Sony AI, WASPAA 2025) showed LLMs can predict EQ and reverb settings zero-shot. Results improved when the prompt also included DSP features, the effect's parameter definitions and a few worked examples.
- **Context you already have:** `MasterAssistantReport` (LUFS, crest factor, correlation) plus spectral centroid and flatness from the analyser, and the `ParamInfo` list as a JSON schema.
- **Model:** a 1–2B instruct model in GGUF via `llama-cpp-2`, or ONNX via `ort`, downloaded on first use rather than bundled. Ask for JSON output and clamp every value against `ParamInfo` before applying it.
- **Expect weaker results** than large models; in the paper, quality rose with model size. Keep the edits small (a few dB) and always apply them through `edit_suite`, so they can be undone.

An optional cloud fallback (a hosted API) would give much better text understanding, but it breaks the app's offline, no-account design. Make it opt-in if you add it.

## Risks and open questions

| Risk | Likelihood | Fallback |
| --- | --- | --- |
| AFx-Rep weights aren't licensed for commercial use | Unknown until Phase 0 | Retrain AFx-Rep yourself with the Apache 2.0 code on licensed or self-produced audio, using your own suite as the effect generator. Or skip the model and use a DSP feature distance (spectrum, crest factor, width per band). |
| ONNX export fails (unsupported ops, BEATs backbone quirks) | Medium | Move the front-end transform into Rust; try opset 18; or export only the encoder trunk. |
| Too slow on CPU (over 90 s) | Medium | Shorter excerpts (2 s), fewer dimensions, a smaller population, or disable the spectral modules during the search. |
| Optimiser finds odd settings that score well but sound bad | Medium | Stronger regularisation, tighter per-parameter ranges, and the listening-test set in Phase 5. |
| Embedding ignores loudness and low end | Low–medium | Already handled by loudness matching; add a low-band energy term to the distance if bass translation drifts. |

**Open questions**

- Which sample rate and clip length does AFx-Rep expect? Check in Phase 0.
- Should the match touch the Stem EQ and Stem Rebalance modules when stems are separated? Possibly a later step.
- Should **Match reference (AI)** replace Match EQ's *Learn from reference track* in the UI, or sit next to it?

## Milestone checklist

About 10–13 working days for Phases 0–5. Phase 0 is the go/no-go gate.

- [ ] Confirm the AFx-Rep weights licence (go/no-go)
- [ ] Reproduce ST-ITO on the demo track in Python
- [ ] Export `afx_rep.onnx` and pass the parity check (cosine > 0.999)
- [ ] Record the CPU time per 10 s clip
- [ ] Add `ml_style.rs` with `embed()` and `distance()`
- [ ] Add the model to LFS and document it in `models/README.md`
- [ ] Write `style_space.rs` encode/decode plus the round-trip test
- [ ] Write `style_match.rs` CMA-ES plus the mock-convergence test
- [ ] Add the UI button, progress, cancel and change report
- [ ] Apply the result through `edit_suite` as a single undo step
- [ ] Pass the known-settings recovery test (50% closer, EQ within 2 dB)
- [ ] Add a `style_eval` case to `bench_modules`
- [ ] Write up the 10-pair listening test in `docs/`
- [ ] Update the README features list

## Sources

- [ST-ITO paper (arXiv 2410.21233)](https://arxiv.org/pdf/2410.21233), code: github.com/csteinmetz1/st-ito (Apache 2.0)
- [LLM2Fx, Sony AI](https://www.ai.sony/publications/Can-Large-Language-Models-Predict-Audio-Effects-Parameters-from-Natural-Language/)
- [DeepAFx-ST](https://www.GitHub.com/adobe-research/DeepAFx-ST) (Adobe Research License, non-commercial)
- [Diff-MST](https://ismir2024program.ismir.net/poster_299.html) (CC BY-NC-SA 4.0)
