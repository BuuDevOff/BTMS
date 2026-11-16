# Security policy

## Supported versions

BTMS is maintained on its latest release. Security fixes go into the next release and are not
backported.

| Version | Supported |
|---|---|
| 1.0.x (latest) | Yes |
| Older | No |

## Reporting a vulnerability

Please **do not open a public issue** for a security problem.

Report it privately through GitHub:
[open a private security advisory](https://github.com/BuuDevOff/BTMS/security/advisories/new)
(the "Report a vulnerability" button on the
[Security tab](https://github.com/BuuDevOff/BTMS/security)).

Include, as far as you can:

- the BTMS version (`btms --version`), how you installed it (installer, portable zip or built from
  source) and your Windows version;
- what an attacker can do, and what they need first (a web page you visit, a resource file you
  render, local access…);
- the steps to reproduce it, or a proof of concept;
- any idea you have of a fix.

Never include decryption keys, `magic.dat`, game files or protected resource files in a report.
They are not needed to describe a problem in BTMS.

BTMS is maintained on a volunteer basis. You will get a reply as soon as possible, usually within a
few days. Once the problem is confirmed, a fix is prepared in the private advisory and released,
and the advisory is then published with credit to you, unless you prefer to stay anonymous.

## Scope

### The local interface

`btms interface` (also what runs with no command) starts a small HTTP server for the interface
page. It is designed so that only that page, on the same machine, can drive it:

- it listens on `127.0.0.1` only, never on other network interfaces;
- it answers only when the `Host` header is `127.0.0.1` or `localhost` on its own port, which
  refuses pages of other sites that point a domain name at `127.0.0.1` (DNS rebinding);
- every API request must carry a token drawn at random when the server starts (header
  `X-BTMS-Token`). Only the page served by that run is given it, so a page of another site cannot
  drive the interface (cross-site requests);
- nothing the page sends goes through a command interpreter: a job is the BTMS executable started
  with separate arguments, and a folder is opened by the file explorer given its path.

A way around any of these is in scope: another site or another user of the machine starting a job,
reading files through the server, changing settings, or running a command.

### Files BTMS reads

BTMS parses files it does not control: the archives of the game, and the map files of FiveM
resources (`.ymap`, `.ytyp`, `.ydr`, `.ydd`, `.yft`, `.ytd`), which may come from third parties.
A crafted file that makes BTMS write outside its output folder, or run code, is in scope. A crafted
file that only makes a render fail with an error is a normal bug: please open an issue instead.

BTMS never writes to the game folder. A case where it does is in scope.

### The viewer page

The `index.html` page written next to `tiles.json` displays the tiles of that folder. A way for a
crafted `tiles.json` or tile folder to run script in that page is in scope.

### The installer and the release files

The program itself never connects to the internet. The only download is the installer's keys task
("Set up the keys now", ticked by default): it fetches `magic.dat` over HTTPS from CodeWalker's
official GitHub repository into the setup's temporary folder, then runs `btms import-keys` on it as
the user who started the setup, even in an all-users install. `import-keys` checks every key and
table against its known SHA-1 and refuses a file that does not match, so a modified `magic.dat`
gives no keys. A way for that download to make the installer or BTMS write somewhere else, store
keys that were not checked, or run code is in scope.

The release files (`BTMS-Setup-<version>-x64.exe`, `BTMS-<version>-x64-portable.zip`) are built by
the repository's GitHub Actions workflow from the tagged source, and `SHA256SUMS.txt` lists their
checksums. They are not code-signed yet, so Windows SmartScreen may warn on the first launch. A
release file that does not match its checksum, or a way to change what the workflow builds, is in
scope.

### Out of scope

- Attacks that need administrator rights, or control of the user's account, before they start.
- Problems in GTA V, FiveM or CodeWalker themselves, including the content of CodeWalker's
  repository.
- The decryption keys stored under `%LOCALAPPDATA%\BTMS\keys`. They are the user's own, decoded
  with their own copy of the game, and are protected by the permissions of their Windows account.
- Requests to read resources protected by the FiveM asset escrow (`FXAP`). BTMS reports them and
  leaves them out on purpose. This is not a vulnerability, and BTMS will not circumvent it.
