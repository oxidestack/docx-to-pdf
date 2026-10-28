# Security policy

docx-to-pdf opens documents it did not write, from clients it may not trust, so a vulnerability in it matters.
Please report one privately, and give us the chance to fix it before it is public.

## Reporting a vulnerability

Report it through GitHub's private vulnerability reporting: on the repository's **Security** tab, choose
**Report a vulnerability**, or go straight to
[the form](https://github.com/oxidestack/docx-to-pdf/security/advisories/new). Only the maintainers see the report.

**Do not open a public issue, discussion or pull request for a vulnerability.**

What helps us most:

- the version: the image tag or digest, or the commit you built;
- how it is deployed: `docker run`, Docker Compose or the Helm chart, and the settings you changed;
- what an attacker sends and what happens: the request, and the document if one is needed (attach it to the report,
  never to a public issue);
- what it lets an attacker do: read a file, reach the network, exhaust memory or CPU, take down the service.

## What happens next

We answer in the report's thread, confirm the problem or explain why it is not one, and agree with you on a fix and a
date to disclose it. The fix is released with a GitHub security advisory, a CVE where one applies, and an entry in
the release notes; you are credited in the advisory unless you would rather not be.

## Supported versions

Security fixes go into the latest release. Upgrade to it to get them; earlier releases are not patched.

## What is in scope

- The service: its HTTP interface, its limits and time limits, how it runs and replaces LibreOffice.
- The container image and the Helm chart as published: the user, the file system, the capabilities, the network a
  document can or cannot reach.

A vulnerability in LibreOffice itself belongs to the
[LibreOffice project](https://www.libreoffice.org/about-us/security/). If a document reaches it through docx-to-pdf,
report it here too: the image may need a newer LibreOffice, or the service a limit that keeps the document out.
