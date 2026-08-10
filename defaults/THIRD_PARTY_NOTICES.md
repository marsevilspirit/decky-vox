# Third-party notices

Decky Vox is distributed under the BSD 3-Clause License. It includes or is
designed to run with the following third-party work.

## Decky Plugin Template

- Project: `SteamDeckHomebrew/decky-plugin-template`
- License: BSD 3-Clause
- Use: plugin manifest, build layout, and Loader entry-point structure
- License text: `licenses/decky-plugin-template-BSD-3-Clause.txt`

## decky-voxtype

- Project: <https://github.com/mimed95/decky-voxtype>
- Audited revision: `cbb2201dcad36cf5291ec360c9fb1183fe9071be`
- License: BSD 3-Clause
- Copyright: 2024 mimed95
- Use: selectively adapted controller-listener, SteamOS environment, voxtype
  lifecycle, and binary-bundling patterns. Decky Vox does not copy the whole
  upstream project and implements its own Rust service and native Steam text
  output policy.
- License text: `licenses/decky-voxtype-BSD-3-Clause.txt`

## voxtype

- Project: <https://github.com/peteonrails/voxtype>
- Bundled version: `v0.6.5`, Linux x86-64 AVX2 and Vulkan binaries
- License: MIT
- Copyright: 2025 Peter Jackson
- Use: local microphone capture and whisper.cpp transcription engine
- License text: `licenses/voxtype-MIT.txt`

## whisper.cpp

- Project: <https://github.com/ggerganov/whisper.cpp>
- License: MIT
- Copyright: 2023-2026 The ggml authors
- Use: local Whisper inference embedded in the bundled voxtype binaries and
  source of the multilingual GGML model files
- License text: `licenses/whisper.cpp-MIT.txt`

Whisper model weights are downloaded only after an explicit user action. The
download URL and SHA-256 digest are pinned in Decky Vox; model data is not
included in the plugin ZIP.
