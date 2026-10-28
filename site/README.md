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

## The checks

[starlight-links-validator](https://github.com/HiDeoo/starlight-links-validator) fails the build on an internal link
to a page that does not exist or to an anchor that is not a heading of its page, and the staging fails on a link to a
file that is not in the repository. The CI builds the site on every push.

## Where it is served from

`oxidestack.github.io` is shared by every project of the organisation, so this site is a project page under
`/docx-to-pdf/`: that is `base` in `astro.config.mjs`, and every internal link carries it. A GitHub Actions workflow
builds `site/dist/` on a push to `main` and deploys it with GitHub Pages; the repository's *Settings → Pages → Source*
is *GitHub Actions*.
