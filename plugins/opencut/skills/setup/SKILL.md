---
name: setup
description: Install the platform runtime and select the SQLite workspace before connecting OpenCut MCP and Web.
---

# OpenCut Plugin setup

This API-only launcher requires the matching runtime supporting
`--endpoint … --credential-file … mcp serve`; the published beta.6 runtime does
not support this mode. Do not install this candidate over that older runtime.
Start the selected Web/API service in the user's ordinary local terminal before
loading the plugin. The launcher reads connection files but does not start a
Worker or open SQLite. Reading those files and connecting to the configured
loopback API must be permitted by the client. Browser access alone is insufficient.
Do not request database write access for an API-only chat or fall back to local
stdio execution when the API is denied. Service installation/start/stop remain
local administration outside an API-only chat.

Installing the plugin registers its instructions and MCP launcher; it does not
install the native editor or select an existing video project automatically.
If MCP cannot start, finish runtime installation before retrying editing tools.
This plugin needs a local desktop chat and access to local files and processes.
An account installation does not give a cloud or mobile chat access to the computer.
For a standalone Codex installation, follow the repository's OpenCut Desktop
instructions for an available matching release. The desktop installer verifies
the package, uses bundled Node, starts the selected service, and attempts to
register `opencut_desktop`. Confirm the reported registration status and verify
the project list from a new local task. An installed runtime can have a failed
or disabled MCP registration.
An `Operation not permitted` error from a local probe does not establish that
this is a cloud session or that the runtime is missing. Before declaring setup
blocked, follow "Recover a denied local check" in `../edit/SKILL.md`: distinguish
host permission limits from service failures and application access denials,
and use the host's supported approval flow for one bounded read-only retry when
permitted. Never use that flow to bypass an OpenCut authorization decision.

1. Read the platform support table in the repository README. Download the
   matching platform ZIP from https://github.com/miracle-ai-studio/OpenCut-Plugin/releases.
   For a personal package, use the exact `runtime_release_url` recorded in
   `distribution.json`; an instructions-only plugin update may retain its
   previously verified runtime release. For a platform package, use its bundled
   runtime and `release.json`. Do not substitute an unrelated stable release.
   The `-personal.zip` is the portable plugin for personal account import;
   the platform ZIP also includes the native runtime required by its launcher.
   A source checkout or GitHub marketplace plugin alone has no runtime archive.
2. Follow that release's prerequisites and checksum instructions. Extract its
   bundle, then run `node scripts/install.mjs` inside the extracted bundle.
   For an existing workspace, use `node scripts/install.mjs --database /absolute/path/workspace.sqlite`.
   Preserve the selected database and media; do not substitute a new empty
   workspace or migrate user files merely to make connection succeed.
3. Run `node scripts/doctor.mjs`. A missing install.json means runtime setup is
   incomplete. A native runtime failure must be resolved before promising
   thumbnails, preview or export. Do not override resource admission limits.
4. Install/reload the plugin using the supported desktop Plugins interface or
   marketplace command reported by the installer. Do not edit the plugin cache.
5. In the ordinary local terminal, from the actual plugin directory, run
   `node scripts/web.mjs status`. Reuse the matching ready service; if stopped,
   run `node scripts/web.mjs start` once for the same configured database. These commands return service information;
   do not assume they open a browser or communicate its address to the user.
   MCP, Web, and the default local CLI use the database saved in install.json.
   To change it, stop the Web service with the original configuration before
   running the installer with `--database`, then reload the plugin. Verify the
   returned database and executable identity, `ready: true` and non-null
   `web_url`, and check that `<endpoint>/projects` serves the editor page.
   Immediately send that observed URL as a clickable Markdown link, before
   starting any editing work. Never reuse a remembered port or present an
   unavailable service as ready; surface a failed check instead of retrying
   startup blindly. Follow the edit skill's startup flow on later editing tasks.
6. Inspect project IDs and runtime capabilities through the connected MCP tools
   before editing. Confirm the intended project is present; never claim a skill
   being visible proves its MCP server is connected. After selecting or creating
   a project, provide `<endpoint>/editor/<URL-encoded observed project ID>`;
   append a scene query only for a scene observed in that project.

Installation paths are local configuration. Keep install.json, databases, media,
credentials and diagnostic outputs outside the repository and release archive.
