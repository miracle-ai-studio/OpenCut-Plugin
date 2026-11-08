OpenCut 0.0.1-beta restarts the release numbering for the Rust local editor, native render worker, desktop launcher and MCP integration.

- The current release requires macOS 26 or later on Intel, matching the deployment target of its bundled FFmpeg libraries and executables. It contains the native runtime and retained third-party licenses and source provenance.
- The installer selects the current Mac architecture and verifies the package checksum. Runtime locations are discovered at installation, without a developer's machine paths.
- CLI, Web and MCP share the selected workspace. The desktop launcher connects MCP to the running local API.
- The README's Codex install command downloads the versioned release, starts the local service, registers `opencut_desktop`, and installs and enables **OpenCut Plugin**. Start a new local Codex conversation after installation.
- For an existing 0.5.0 beta installation, stop its service, then use `sh install-desktop.sh install v0.0.1-beta`. This explicit numbering reset retains the selected workspace. Subsequent upgrades use `update`.
- Jumping to the end of the timeline during playback or after pausing stops the audio preview and leaves the playhead at the end. The time field now shows the accepted format, with an example in seconds and a six-decimal-place limit. These behaviors passed scoped Chrome checks on an isolated project.

Apple Silicon validation is deferred until compatible hardware is available and is not a prerequisite for this Intel release. Publication requires the selected platform packages and their native acceptance jobs. Ad-hoc signing is not Developer ID signing or notarization. A prerelease and scoped checks do not imply full product qualification.
