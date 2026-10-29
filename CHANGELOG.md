# Changelog

<!-- markdownlint-configure-file { "MD013": false, "MD024": { "siblings_only": true } } -->

All notable changes to docx-to-pdf, newest release first. The layout is
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/)'s, with sections for the people who run the service: Action
required, Features, Bug fixes, Performance and Other changes. The versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

The changes since the last release are the release notes of the commits after its tag; a release gathers them into
its section below.

## [1.0.0] - 2026-10-29

### Bug fixes

- The classic Helm repository moves to https://github.com/oxidestack/docx-to-pdf/releases/latest/download. (release, 8be8ee2)

### Performance

- An instance now returns the memory a conversion freed to the system before it answers, so a pod converting picture-heavy documents sits about 50 MiB lower between conversions. Peak memory and the sizing formula are unchanged. (instance, 04ebdc7)

[1.0.0]: https://github.com/oxidestack/docx-to-pdf/releases/tag/v1.0.0
