# Changelog

## [0.8.0](https://github.com/CCherry07/pi-rs/compare/v0.7.0...v0.8.0) (2026-09-04)


### Features

* align Hermes memory behavior ([5ebcb84](https://github.com/CCherry07/pi-rs/commit/5ebcb84849941f3e71e10f637da272be8a842fd7))
* **cli:** add RPC, ACP, and session migration ([065c012](https://github.com/CCherry07/pi-rs/commit/065c0125862dbaa63b3e42297b423957cd70317a))
* **cli:** add session import export and share ([c7c9123](https://github.com/CCherry07/pi-rs/commit/c7c9123e3b1c16d31411f5cc9d8133bfdf8116d2))
* **extensions:** expand Pi runtime compatibility ([dece4ca](https://github.com/CCherry07/pi-rs/commit/dece4ca6ae1c2c61ffcd513fbc9a230de3da43f2))
* **extensions:** support dynamic provider registration ([8dcfca4](https://github.com/CCherry07/pi-rs/commit/8dcfca49cb3537b5bc3adef8944c1a039b70c783))
* **plugins:** add Hermes memory and subagent workflows ([0ebee3e](https://github.com/CCherry07/pi-rs/commit/0ebee3ec2298f5148740f72eba0dfcb64339d2f9))
* **plugins:** add rust-first session transfer ([e839807](https://github.com/CCherry07/pi-rs/commit/e839807a752f1dff5ee3ddc949e145c9744d2a18))
* **providers:** expand Pi provider coverage ([38ef6f7](https://github.com/CCherry07/pi-rs/commit/38ef6f77675f0202f08891fcb946f5534deaacad))
* **settings:** implement current non-UI runtime settings ([6a4cbcb](https://github.com/CCherry07/pi-rs/commit/6a4cbcb7a768d639ab61b59bf9eb3a5ccceae8d4))


### Bug Fixes

* **memory:** fall back from empty mutation batches ([7eb5715](https://github.com/CCherry07/pi-rs/commit/7eb5715722094ff989b3d2b4e4bca9f05aa3a8e4))
* **session:** align token accounting with Pi ([79582c1](https://github.com/CCherry07/pi-rs/commit/79582c105d95df1429ccc2e65430ee063fa8b9c6))
* **tools:** prefer explicit edit replacements ([a911fb0](https://github.com/CCherry07/pi-rs/commit/a911fb012d86922a8fb61f2f74aefdae3f0c9342))

## [0.7.0](https://github.com/CCherry07/pi-rs/compare/v0.6.0...v0.7.0) (2026-08-26)


### Features

* align agent runtime with Pi behavior ([b9e3bc5](https://github.com/CCherry07/pi-rs/commit/b9e3bc5fcf6489b8ef02dd170d7805fc27a099de))
* route agent plugin hooks by derived interests ([616063b](https://github.com/CCherry07/pi-rs/commit/616063b74205999cde605c405663375c5ca60737))

## [0.6.0](https://github.com/CCherry07/pi-rs/compare/v0.5.1...v0.6.0) (2026-08-26)


### Features

* expand JavaScript extension and TUI compatibility ([2e1b050](https://github.com/CCherry07/pi-rs/commit/2e1b050737c862241fe3ea7066ac8bbf987b3b2a))

## [0.5.1](https://github.com/CCherry07/pi-rs/compare/v0.5.0...v0.5.1) (2026-08-26)


### Bug Fixes

* align Pi extension module compatibility ([9c6f5cc](https://github.com/CCherry07/pi-rs/commit/9c6f5cc28af18b19c106d3b4e2b6459c36c5392e))

## [0.5.0](https://github.com/CCherry07/pi-rs/compare/v0.4.1...v0.5.0) (2026-08-25)


### Features

* expand Pi-compatible extension runtime ([7ebb29d](https://github.com/CCherry07/pi-rs/commit/7ebb29d1cf8204bbe348cb63c8b68d3bc43662bf))


### Bug Fixes

* **release:** wait for npm registry propagation ([85d1d7f](https://github.com/CCherry07/pi-rs/commit/85d1d7f8a61396c4f58ee03ea0f3552bf15e506d))

## [0.4.1](https://github.com/CCherry07/pi-rs/compare/v0.4.0...v0.4.1) (2026-08-24)


### Bug Fixes

* **tui:** keep working indicator active during tools ([749dfae](https://github.com/CCherry07/pi-rs/commit/749dfae9c912ec863dad9ff4afe2556b7e31b709))

## [0.4.0](https://github.com/CCherry07/pi_rs/compare/v0.3.0...v0.4.0) (2026-08-24)


### Features

* **packages:** add JavaScript package management ([cf47850](https://github.com/CCherry07/pi_rs/commit/cf47850eeb39ba992e2a7e93c940a93e71e01c82))


### Bug Fixes

* **package:** handle missing native npm packages ([6415f88](https://github.com/CCherry07/pi_rs/commit/6415f884f304d0c1c2bd0d6273ddb81bd4a9afc0))

## [0.3.0](https://github.com/CCherry07/pi_rs/compare/v0.2.1...v0.3.0) (2026-08-24)


### Features

* **models:** support Pi models.json configuration ([8839680](https://github.com/CCherry07/pi_rs/commit/88396800a7ae4fca1bea3d7fb754522bea2b83c4))
* **providers:** sync Google and Anthropic catalogs ([53685ac](https://github.com/CCherry07/pi_rs/commit/53685ac6e87e1e3556948c8c62be884c695fe50c))


### Bug Fixes

* **session:** preserve explicit custom model selections ([67b1cbf](https://github.com/CCherry07/pi_rs/commit/67b1cbfcdf86084f65eee3f2f9dc5a5676e6793b))

## [0.2.1](https://github.com/CCherry07/pi_rs/compare/v0.2.0...v0.2.1) (2026-08-23)


### Bug Fixes

* make releases version independent ([7dec46f](https://github.com/CCherry07/pi_rs/commit/7dec46fe10e526d78f1741ab0df9bf036e4a43b5))

## [0.2.0](https://github.com/CCherry07/pi_rs/compare/v0.1.0...v0.2.0) (2026-08-23)


### Features

* add provider authentication and Pi-aligned tools ([5f7fed2](https://github.com/CCherry07/pi_rs/commit/5f7fed209d671d0848607f788d49e38adc80761c))

## 0.1.0 (2026-08-23)


### Features

* add native plugin package management ([62662fa](https://github.com/CCherry07/pi_rs/commit/62662fa005fdba21adafda05eceec40d0e0b0bab))
* add native plugin support and refine TUI integration ([db705fa](https://github.com/CCherry07/pi_rs/commit/db705fa0f016aca77a1649663c1cf1477844784a))
* add README ([3d57d71](https://github.com/CCherry07/pi_rs/commit/3d57d71e6747ad4ccb8f5bb4a3bb703278047c7d))
* add TypeScript extension host ([ec41d26](https://github.com/CCherry07/pi_rs/commit/ec41d2655a3a14819a4bb8c6c3c66eaf04c2d531))
* establish Pi-compatible Rust agent baseline ([12d6b9a](https://github.com/CCherry07/pi_rs/commit/12d6b9a05e4d676c009b5e026aa1c823c951ff0c))
* restructure frontend and automate releases ([3b0ec73](https://github.com/CCherry07/pi_rs/commit/3b0ec73b4bb653304aba3b8f4f3be8954509d61b))

## Changelog

Release Please maintains this file from Conventional Commits merged into `main`.
