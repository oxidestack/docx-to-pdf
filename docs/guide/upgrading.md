# Upgrading

How to move a deployment from the release it runs to a newer one. Find the section of every release newer than
yours, and apply them oldest first: going from 1.0.0 to 1.2.0 means the section of 1.1.0, then the one of 1.2.0. A
release with nothing to change says so. Then [upgrade](#how-to-upgrade) to the newest release in one step: a release
never needs an intermediate one installed first.

Each section lists what the release asks of a deployment or of a client: under **Action required**, a change to make
before or with the upgrade; under **Deprecated**, something that still works and will stop in a later release; under
**Removed**, something that has stopped. The [changelog](https://github.com/oxidestack/docx-to-pdf/blob/main/CHANGELOG.md)
says everything else each release changed. Only releases have a section: a release candidate (`-rc.N`) is described
on its GitHub release, and what it changes is written here under the release that ships it.

The documentation of the release you run is on this site too: the version picker at the top of each page switches
between releases, and a page of an older release links the same page in the latest.

## How to upgrade

docx-to-pdf keeps nothing between conversions: it has no database and no files to migrate, so an upgrade replaces
the containers and nothing else, and a rollback is the same step with the older version.

1. Read the sections below for every release newer than yours, and make their changes to your configuration, chart
   values or clients.
2. Check the new image or chart, if you verify releases ([Verifying a release](installation.md#verifying-a-release)).
3. Replace the containers:
   - `docker run` or Docker Compose: change the image's tag (`quay.io/oxidestack/docx-to-pdf:<version>`) and start it
     again (`docker compose up -d`).
   - Helm: run the same `helm upgrade --install` as [Installation](installation.md#with-helm), with the new
     `--version` and your own values file. The chart starts each new pod before it stops an old one; a new pod takes
     conversions once `/readyz` answers `200`, and an old one finishes the conversions it has before it stops
     ([Operations](operations.md#stopping)).
4. Watch the first minutes: `/readyz`, the answers' statuses and the conversion metrics
   ([Operations](operations.md#telemetry)). To roll back, run step 3 with the version you had.

## Upgrading to 1.1.0

Nothing to change: the image and the chart upgrade in place from 1.0.0.
