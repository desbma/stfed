# Changelog

## 1.1.1 - 2026-07-15

### <!-- 01 -->💡 Features

- Normalize folder paths ([f9f5282](https://github.com/desbma/stfed/commit/f9f52821671dd563a29fb008701d6547ea965dc1) by desbma)
- Use RUST_LOG env variable ([39a8c5c](https://github.com/desbma/stfed/commit/39a8c5cf7c3d5c24f3747618edfcb985930ac1d7) by JackGlobetrotter)
- Default log levels ([a9173f2](https://github.com/desbma/stfed/commit/a9173f20f4fa8b4a101e83ee6dd3292aefbf8bcf) by desbma)

### <!-- 02 -->🐛 Bug fixes

- Elided lifetime compile error ([780c61f](https://github.com/desbma/stfed/commit/780c61ffb6c2b6e7a6e8c63e81a8b677038df71b) by desbma)
- Path normalization from config ([a32a365](https://github.com/desbma/stfed/commit/a32a365729e77331ba640c5f045da6981df6f55f) by desbma)
- Msrv ([a7e3e17](https://github.com/desbma/stfed/commit/a7e3e17b8056f1b39596982b44eb3ebbcd5a3c94) by desbma)
- Don't replay buffered events on startup (see #11) ([491f3d3](https://github.com/desbma/stfed/commit/491f3d3ce8dec4fd5f0b7645816f57856f6186c1) by desbma)
- Read event by chunks to avoid loss during bursts (see #11) ([8dea4cd](https://github.com/desbma/stfed/commit/8dea4cdb018aa3ab141bb720849f87b73478e24b) by desbma)
- Reuse event cursor after a reconnect (see #11) ([cf8ab9a](https://github.com/desbma/stfed/commit/cf8ab9a5197206e7176e6326c7a733d2c92e8c28) by desbma)
- Dispatch file down sync done event only if successful and file (see #11) ([74a7ef1](https://github.com/desbma/stfed/commit/74a7ef1ca1952c2d3b804c312bda3376314461e2) by desbma)
- Reject empty hook command when parsing hooks ([dae91f6](https://github.com/desbma/stfed/commit/dae91f67b9902e267886aacbfef53bad034f367e) by protagonista-design)
- Failed hook state handling (see #13) ([ea283e1](https://github.com/desbma/stfed/commit/ea283e1a8f70705a6c3ee175fc560c4cf0d4c3c8) by desbma)
- Invalid event path handling (see #15) ([3adfbec](https://github.com/desbma/stfed/commit/3adfbec421b10f2781c750331dd9513b8a89a0a5) by desbma)

### <!-- 04 -->📗 Documentation

- Update changelog template ([b6c04c7](https://github.com/desbma/stfed/commit/b6c04c7c4b872d6eb840ec89f637b6053c612835) by desbma)

### <!-- 05 -->🧪 Testing

- Add some unit tests ([b4b9cc3](https://github.com/desbma/stfed/commit/b4b9cc3df031f445480df9c139d9e80c1030388c) by desbma)

### <!-- 06 -->🚜 Refactor

- Use Option::transpose ([e92e523](https://github.com/desbma/stfed/commit/e92e5238e16a8119c187c2190c9a5e074310ecbb) by desbma)
- Lazy anyhow context ([4d63889](https://github.com/desbma/stfed/commit/4d63889af98b1714d6fe1b876f2bc529884ae5d4) by desbma)
- Use more idiomatic Deref for NormalizedPath to &Path ([39512c6](https://github.com/desbma/stfed/commit/39512c6b8f3ff3b402f95efafbb1c8819919fc84) by desbma)
- Make FolderHook non cloneable (see #14) ([d92fa3a](https://github.com/desbma/stfed/commit/d92fa3a3942d8c8bc459e5dae2cc0390c5d437b2) by desbma)

### <!-- 09 -->🤖 Continuous integration

- Update workflow ([c9e0ebd](https://github.com/desbma/stfed/commit/c9e0ebd3ab0e90672fa46202e8b3619c58b0326b) by desbma)
- Pin gh actions versions with hash ([287f2f8](https://github.com/desbma/stfed/commit/287f2f8d0ae53176fb9dde31f9e3e419333babea) by desbma)
- Update actions ([1a2839f](https://github.com/desbma/stfed/commit/1a2839f469e09d487b774111f9445a1dde23eb82) by desbma)

### <!-- 10 -->🧰 Miscellaneous tasks

- Update lints ([4b48cb5](https://github.com/desbma/stfed/commit/4b48cb54bfdfed61cbc48af699cde40b47d49fff) by desbma)
- Update pre-commit hooks ([1a6be64](https://github.com/desbma/stfed/commit/1a6be6456a7890342594b0ea973a3d0d86c4bc42) by desbma)
- Update dependencies ([f9851f5](https://github.com/desbma/stfed/commit/f9851f5fedb441764c560ce32bc5223e87ce56cb) by desbma)
- Fix lint ([2a7d17a](https://github.com/desbma/stfed/commit/2a7d17aba7208c3549981513511b32718f5e8d49) by desbma)
- Lint ([3ffc68d](https://github.com/desbma/stfed/commit/3ffc68d71d1d05ebac4e6e9ab7de04fe2cb951bf) by desbma)
- Remove pre-commit hooks ([3337007](https://github.com/desbma/stfed/commit/33370073ee3671a32f47619f270128920b3cf5c3) by desbma)
- Update dependencies ([b912ea6](https://github.com/desbma/stfed/commit/b912ea6cc0ec19fd295a2e7f1d6122981f7759ad) by desbma)
- Add AGENTS.md + conform to it ([97832c1](https://github.com/desbma/stfed/commit/97832c18a5ac1a905b640e6ada784a6c76b71083) by desbma)
- Update lints ([600004e](https://github.com/desbma/stfed/commit/600004ea91ae6028226f6c7fbebd42f846a6fc76) by desbma)
- Update msrv ([0a6d790](https://github.com/desbma/stfed/commit/0a6d790a40c73ad8097cf71c0844c655a1294b4d) by desbma)
- Update release script ([a08b780](https://github.com/desbma/stfed/commit/a08b780f1629ad11822665e6b7a2f72e8f547952) by desbma)

______________________________________________________________________

## 1.1.0 - 2024-11-06

### <!-- 01 -->💡 Features

- Support remote file conflict hook ([f930f66](https://github.com/desbma/stfed/commit/f930f661f08143331e2fb0b31340814b0d403878) by desbma)

### <!-- 06 -->🚜 Refactor

- Simplify FolderHookId type ([f5868e8](https://github.com/desbma/stfed/commit/f5868e8f90526dca0ed913756f3fb296a993fff5) by desbma)

### <!-- 10 -->🧰 Miscellaneous tasks

- Enable more lints ([d77c857](https://github.com/desbma/stfed/commit/d77c857ebbebe8d7eba4ff93fe314b2476216d03) by desbma)
- Update release script ([972edcf](https://github.com/desbma/stfed/commit/972edcfc9e7b250a839467f832f3bebbbc45ee26) by desbma)

______________________________________________________________________

## 1.0.4 - 2024-09-29

### <!-- 02 -->🐛 Bug fixes

- Http timeout firing before API timeout ([ed8d17c](https://github.com/desbma/stfed/commit/ed8d17cb6b6c5de60390116b7b23411bfccc42c8) by desbma)

______________________________________________________________________

## 1.0.3 - 2024-09-12

### <!-- 02 -->🐛 Bug fixes

- Update for new possible Syncthing config dir ([02336ce](https://github.com/desbma/stfed/commit/02336ceec087f19111650cab7088a4d0c6e59b5e) by desbma)

### <!-- 04 -->📗 Documentation

- README: Add AUR reference ([4db64b5](https://github.com/desbma/stfed/commit/4db64b5d6089bc58ffe719d74f21634598977bba) by desbma)

### <!-- 10 -->🧰 Miscellaneous tasks

- Bump simple_logger dependency ([e533aa4](https://github.com/desbma/stfed/commit/e533aa4ae55b5c034d56d4aca2a063ad7b357623) by desbma)
- Update dependencies ([26bc51c](https://github.com/desbma/stfed/commit/26bc51cb1fe1ab027bd0f64da8a8425191b10749) by desbma)
- Lint ([3dfce86](https://github.com/desbma/stfed/commit/3dfce86ea44f99a168052bf1a65ce82517c766b7) by desbma)
- Add more clippy lints ([f670315](https://github.com/desbma/stfed/commit/f6703154aff7775d38206a552976794b45b73f4e) by desbma)

______________________________________________________________________

## 1.0.2 - 2023-10-19

### <!-- 02 -->🐛 Bug fixes

- Error message spacing chars ([c59d19c](https://github.com/desbma/stfed/commit/c59d19c55c52197182dfaf094f6f2a394b9aef2b) by desbma)

### <!-- 10 -->🧰 Miscellaneous tasks

- Lint ([16b7383](https://github.com/desbma/stfed/commit/16b7383a1ec4d6e0e28b00f5b20ded99022ad9df) by desbma)

______________________________________________________________________

## 1.0.1 - 2023-01-10

### <!-- 01 -->💡 Features

- Improve retry logic to also retry if first connection fails ([1ebe996](https://github.com/desbma/stfed/commit/1ebe996ab6b61fb3701244a9aea5abd74e5abca3) by desbma)
- Add git cliff config ([2d556c5](https://github.com/desbma/stfed/commit/2d556c59f7cd4901bd715f7f89cdd81aded47215) by desbma)

### <!-- 02 -->🐛 Bug fixes

- Change system service dependency to syncthing.service ([2f5c142](https://github.com/desbma/stfed/commit/2f5c142393beff19190e3c1d90cfc02bbb5b19ac) by desbma)
- Syncthing config parsed when not needed ([93633b2](https://github.com/desbma/stfed/commit/93633b2c778b0953f1dd07302d86f78180bdc5be) by desbma)
