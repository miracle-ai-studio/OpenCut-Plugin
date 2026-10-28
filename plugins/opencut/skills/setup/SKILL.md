---
name: opencut-setup
description: Install the platform runtime and select the SQLite workspace before connecting OpenCut MCP and Web.
---

# OpenCut Plugin setup

Installing the plugin registers its instructions and MCP launcher; it does not
install the native editor or select an existing video project automatically.
If MCP cannot start, finish runtime installation before retrying editing tools.

1. Read the platform support table in the repository README. Download the
   matching platform ZIP from https://github.com/miracle-ai-studio/OpenCut-Plugin/releases.
   A source checkout or GitHub marketplace plugin alone has no runtime archive.
2. Follow that release's prerequisites and checksum instructions. Extract its
   bundle, then run `node scripts/install.mjs` inside the extracted bundle.
   For an existing workspace, use `node scripts/install.mjs --database /absolute/path/workspace.sqlite`.
   Preserve the selected database and media; do not substitute a new empty
   workspace or migrate user files merely to make connection succeed.
3. Run `node scripts/doctor.mjs`. A missing install.json means runtime setup is
   incomplete. A browser/runtime failure must be resolved before promising
   thumbnails, preview or export. Do not override resource admission limits.
4. Install/reload the plugin using the supported desktop Plugins interface or
   marketplace command reported by the installer. Do not edit the plugin cache.
5. Run `node scripts/web.mjs start` to open the Web editor for that same configured
   database. MCP and Web use `OPENCUT_PLUGIN_DATABASE` when explicitly provided,
   otherwise install.json's database, otherwise the default plugin workspace.
6. Inspect project IDs and runtime capabilities through the connected MCP tools
   before editing. Confirm the intended project is present; never claim a skill
   being visible proves its MCP server is connected.

Installation paths are local configuration. Keep install.json, databases, media,
credentials and diagnostic outputs outside the repository and release archive.
