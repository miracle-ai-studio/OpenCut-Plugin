# OpenCut

[English](README.md) · [简体中文](README.zh-CN.md)

OpenCut is an open-source video editing system with a Web workbench and a local Rust engine. Edit directly in the workbench or let Codex and other MCP clients work on the same project.


## Edit and deliver

- Organize footage, inspect shots, and edit video and audio on a multi-track timeline.
- Add captions, text, graphics, masks, transitions, and keyframes.
- Preview edits, render and export video, and track jobs in the workbench.
- Undo edits, compare project revisions, and move projects between workspaces.
- Automate editing through MCP, CLI, HTTP, and the SDKs. [Operation reference](docs/cli-mcp-capabilities.md)

## Install on macOS

The current `0.0.1-beta` package requires **macOS 26 or later on an Intel Mac**. Apple Silicon validation is deferred.

### Install for Codex

After the [GitHub prerelease](https://github.com/miracle-ai-studio/OpenCut-Plugin/releases/tag/v0.0.1-beta) is public, on an Intel Mac running macOS 26 or later with the Codex CLI installed, paste this block into Terminal. You can also ask a local Codex session to run it.

```sh
(
  installer=$(mktemp) && trap 'rm -f "$installer"' EXIT &&
  curl -fsSL https://github.com/miracle-ai-studio/OpenCut-Plugin/releases/download/v0.0.1-beta/install-desktop.sh -o "$installer" &&
  sh "$installer" install v0.0.1-beta --codex-plugin
)
```

The script verifies the platform package, starts the local service, registers MCP and enables **OpenCut Plugin**. Open a new local Codex conversation, select **OpenCut Plugin**, and ask it to list your OpenCut projects.

### Install the desktop app

Download the `OpenCut-<version>-<chip>.zip` that matches your Mac, unzip it, and double-click `OpenCut.app`. Choose **Install Desktop App** to place the branded app in `~/Applications`, then **Install / Start** and **Open Editor**. The app installs its bundled runtime, starts the local service, and registers `opencut_desktop` when Codex is installed. Existing installations can use **Switch to Standard Workspace** to select the existing `~/.opencut/workspace.sqlite` database. Projects in the previous workspace remain there and its database is kept. The window displays the current Metal device status. To upgrade a running service, choose **Stop Service** before **Install / Start**. A public download with only local ad-hoc signing may require a manual macOS trust decision; a Developer ID signed and notarized release requires an Apple Developer account.

After installation, opening OpenCut starts a stopped service automatically and reuses a ready service. **Install Desktop App** also replaces an existing OpenCut app of the same or an older version after checking the staged copy. The MCP entry is `opencut_desktop`, using stdio with the fixed executable `~/.opencut-plugin/opencut-mcp` and no arguments or environment variables. Open OpenCut before connecting your MCP client.

You can also install from a downloaded platform ZIP with its `.sha256` file beside it. From a source checkout, use `scripts/install-desktop.sh` in place of `install-desktop.sh`.

```sh
sh install-desktop.sh install --archive /path/to/opencut-release.zip
```

Open the `web_url` in the installer output. To edit with an agent, open a new local Codex session and ask it to list your OpenCut projects. New installs use `~/.opencut/workspace.sqlite`. Upgrades keep the selected workspace.

## Manage your installation

| Command | Purpose |
|---|---|
| `sh install-desktop.sh start` | Start the workbench and print its URL |
| `sh install-desktop.sh check` | Check the service, project list, and Codex MCP registration |
| `sh install-desktop.sh doctor` | Verify runtime integrity, architecture, and basic dependencies |
| `sh install-desktop.sh stop` | Stop the local service |
| `sh install-desktop.sh update v0.0.1-beta` | Install the selected version and start the service again |

Finish active jobs and run `stop` before updating. `update --archive /path/to/opencut-release.zip` upgrades from a local ZIP. Run `check` after updating to verify the connection. [Operations guide](docs/operations.md)

The shared registry provides 185 named operations across 12 domains. See the [MCP and CLI operation reference](docs/cli-mcp-capabilities.md) for the available tools.

## Build a desktop package

Use a native macOS build machine for `x86_64-apple-darwin` or `aarch64-apple-darwin`. Prepare the matching media and runtime specs, then run:

```sh
sh scripts/build-desktop-release.sh TARGET native-media.json runtime.json dist/releases
sh scripts/package-desktop-app.sh dist/releases/opencut-plugin-VERSION-TARGET.zip dist/releases
```

The GitHub Actions build runs Intel by default on a configured release runner. Enable Apple Silicon explicitly to build and validate both targets in parallel. [Build setup](docs/development.md) · [Architecture](docs/architecture.md) · [Release status](docs/status.md)

OpenCut is a community fork of [OpenCut](https://github.com/OpenCut-app/OpenCut), maintained by [miracle-ai-studio](https://github.com/miracle-ai-studio). [MIT license](LICENSE)
