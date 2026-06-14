# [1.2.0](https://github.com/dlepaux/multilinguarr/compare/v1.1.0...v1.2.0) (2026-06-14)


### Features

* **detection:** language+role-aware has_base_audio_track ([1c54f9e](https://github.com/dlepaux/multilinguarr/commit/1c54f9ea8c669b3f481a71389868e2aa0eac0928))
* **detection:** parse audio channel count + commentary disposition ([7ab092d](https://github.com/dlepaux/multilinguarr/commit/7ab092d32e3cb0615f9c86fe826a83abd604f20c))
* **handler:** observe-only audio-truth skip counter on import ([5ba1f32](https://github.com/dlepaux/multilinguarr/commit/5ba1f32026f73ca9387360e9d2d6b5c182aa9b93))
* **reconcile:** emit audio-truth skip counter on reconcile path ([8756571](https://github.com/dlepaux/multilinguarr/commit/87565714483368c643127cdb49937560ed2779c1))

# [1.1.0](https://github.com/dlepaux/multilinguarr/compare/v1.0.0...v1.1.0) (2026-04-26)


### Features

* **client:** add ArrError::Conflict variant and classify 409 structurally ([4cb4393](https://github.com/dlepaux/multilinguarr/commit/4cb4393fa076602d56eb6227b5c2c374e18d8fd5))
* **client:** treat 409 as already-existed via AddOutcome (closes cross-instance race) ([ac0f761](https://github.com/dlepaux/multilinguarr/commit/ac0f7619612e33fdf8a05be8307e19d9513b270b))
* **handler:** emit wrong_language_skip_total counter on alternate-instance mismatches ([a503e61](https://github.com/dlepaux/multilinguarr/commit/a503e6139ee9ea73581c643b0fa861eb7a95c502))
* **handler:** instrument language-tag fallback as INFO + counter ([26c26ec](https://github.com/dlepaux/multilinguarr/commit/26c26ec39faf3fe5a4e4429f52f6ed6dfd772c64))
* **observability:** metrics design foundation — buckets, HELP, ffprobe histogram fix, DLQ gauge ([580a87f](https://github.com/dlepaux/multilinguarr/commit/580a87f799ebcc5e704fb04b2a45caa373012f8b))
* **webhook:** surface raw eventType for unknown events + emit bounded counter ([991aab2](https://github.com/dlepaux/multilinguarr/commit/991aab21118b078499b671f47186cfc0d4ad3582))

# 1.0.0 (2026-04-10)


### Features

* initial release ([1c26966](https://github.com/dlepaux/multilinguarr/commit/1c26966fd8f8dabbddd9890798e32f2cb2bef89d))

# Changelog

All notable changes to this project will be documented in this file.
