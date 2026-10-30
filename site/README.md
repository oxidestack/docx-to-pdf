# The documentation site

[Astro Starlight](https://starlight.astro.build) over the documentation this repository already has: the
[README](../README.md) and the guides in [`docs/guide/`](../docs/guide/). It is served as a project page at
<https://oxidestack.github.io/docx-to-pdf/>.

```sh
make site    # site/dist/, built in the pinned node image; a broken internal link or anchor fails it
```

Node runs only in a container: `make site` runs [`build.sh`](build.sh) in the node image that
`tools/lint/images.Dockerfile` pins by digest, and `npm ci` installs exactly [`package-lock.json`](package-lock.json)'s
tree, every package pinned by version and integrity hash. To look at the pages while editing them, run the dev server
in that image, from the repository root:

```sh
docker run --rm -it -p 4321:4321 --user "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD:/src" -w /src/site \
  "$(sh tools/lint/lint.sh --list | awk '$1 == "markdownlint" { print $3 }')" \
  sh -c 'sh build.sh && npm run dev'
```

and open <http://localhost:4321/docx-to-pdf/>.

## A page has one home, and it is not here

Nothing is copied into this directory by hand. Every build runs [`scripts/stage.mjs`](scripts/stage.mjs), which reads
`README.md` and `docs/guide/*.md` and writes them into `src/content/docs/` with three changes and no others:

| Change | Why |
| --- | --- |
| The first `#` heading becomes the page's title | Starlight takes the title from frontmatter |
| `[Configuration](configuration.md#server)` becomes `/docx-to-pdf/configuration/#server`; a link to any other file of the repository becomes a link to it on GitHub | right on GitHub, a 404 on the site, which carries only the documentation |
| `<name>` outside code becomes `&lt;name>` | a renderer reads it as an HTML tag and drops it |

`src/content/docs/`, `dist/`, `.astro/` and `node_modules/` are build output, ignored by git: edit `README.md` or
`docs/guide/…`, never a staged page. A new guide page is staged by being there; it appears in the navigation once
[`astro.config.mjs`](astro.config.mjs)'s `sidebar` names it.

## The Benchmarks page, the README's numbers and the comparison guide's

[`public/benchmarks/results.json`](public/benchmarks/results.json) is the raw output of the latest full run of
`make compare` ([`examples/compare/`](../examples/compare/)), and
[`public/benchmarks/earlier/<stamp>.json`](public/benchmarks/earlier/) those of the earlier full runs, each committed
as it was written and served beside the page for download. One run's figure moves from one virtual machine to the next
more than its own σ says, so the README and the comparison guide quote a ratio, a time or a peak as the range over the
latest run and the earlier runs of its shape: the same mode, blocks, conversions, architecture, vCPU count, chip and
speed cell. An earlier run of another shape stays in `earlier/` and is listed on the Benchmarks page with what
differs, and no range includes it: a new kind of host starts its own ranges, and its first run is quoted as its
ratio ± its standard error. [`scripts/evidence.mjs`](scripts/evidence.mjs) reads them on every build and writes the
Benchmarks page
(the speed and memory table with its charts, the ratios, the rows where Gotenberg is ahead, the torture cases, the
soaks, the machine, the versions and the documents), the README's headline block, which the site shows as its home
page, and the block of `docs/guide/comparison.md` (the run, the ratios, the rows where Gotenberg is ahead and the
torture cases, each row linking the Benchmarks page). Nothing in any of them is typed by hand, and the build fails when
something is:

| Where | What fails the build |
| --- | --- |
| The block between `benchmarks:begin` and `benchmarks:end` of the README and of the comparison guide | any difference from what the results generate: `make evidence` rewrites both |
| The README and the comparison guide outside their blocks | a number with a unit of measure (×, %, s, MiB, MB, conversions), outside code: a setting is written as code (`180s`) |
| The generator itself | a digit in the text of a page that is not a value read from the results, so a figure written into a template |

A new full run is published with `make evidence RUN=.bench/compare/<stamp>`, then `make site`:
[`scripts/publish.mjs`](scripts/publish.mjs) moves the published `results.json` to `earlier/<its stamp>.json`, puts
the run's own `results.json` in its place, and the blocks are rewritten from them all. It refuses, changing nothing, a
quick run, a run not newer than the published one and a stamp already in `earlier/`.

## The checks

[starlight-links-validator](https://github.com/HiDeoo/starlight-links-validator) fails the build on an internal link
to a page that does not exist or to an anchor that is not a heading of its page, and the staging fails on a link to a
file that is not in the repository. Before `npm ci` fetches anything, [`scripts/licences.mjs`](scripts/licences.mjs)
holds every package of the lockfile to `deny.toml`'s licence allowlist, the one the Rust dependencies answer to; the
few build tools outside it (libvips, lightningcss, three BlueOak-licensed helpers, CSS syntax data) are listed there by
name with the licence they were reviewed under: each runs only during the build, and nothing of theirs is in `dist/`.
The CI builds the site on every push.

## Where it is served from

`oxidestack.github.io` is shared by every project of the organisation, so this site is a project page under
`/docx-to-pdf/`: that is `base` in `astro.config.mjs`, and every internal link carries it.
[`.github/workflows/site.yml`](../.github/workflows/site.yml), the `site` job of the GitHub CI, builds `site/dist/` on
every push and deploys it with GitHub Pages on a push to `main`; the repository's *Settings → Pages → Source* is
*GitHub Actions*.

## The logo

`src/assets/docx-to-pdf-mark.svg` (beside the title) and `public/favicon.svg` are the one exception to the rule above:
unchanged copies of `projects/docx-to-pdf/mark.svg` in [oxidestack/artwork](https://github.com/oxidestack/artwork),
where every OxideStack logo is made, since Starlight takes a logo only from a local file. When the mark changes there,
copy it again over both; never edit them here. The README links to the same file there instead of copying it.
