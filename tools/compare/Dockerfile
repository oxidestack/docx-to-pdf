# The runner of `make compare` (tools/compare/compare.sh, examples/compare/compose.yaml). It drives the
# host's Docker through its socket: tools/bench/bench.py and tools/bench/torture.py, standard-library Python, start
# every service, the load generator and the sampler as sibling containers. So the runner needs Python and the docker
# CLI, nothing else. Both images are pinned by the digest of their multi-arch index: the base is tools/bench/
# Dockerfile's, the CLI tools/lint/images.Dockerfile's `docker-cli` stage.
FROM docker:29.5.2-cli@sha256:9ba8e32bfc35a2c7ae2feb1e3241b2778ae21dee80f4dcd31d04e1cfdea86ea2 AS docker-cli

FROM python:3.14.8-slim-trixie@sha256:fbecd461fc72d21b2143c6eaba25143738079154a6d0ea3da3aec724f50eed4a
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
ENTRYPOINT ["python3", "tools/compare/compare.py"]
