OpenCut 0.0.1-beta restarts the release numbering for the Rust local editor, native render worker, desktop launcher and MCP integration.

- The current release targets macOS Intel and contains its matching native runtime and retained third-party licenses and source provenance.
- The installer selects the current Mac architecture and verifies the package checksum. Runtime locations are discovered at installation, without a developer's machine paths.
- CLI, Web and MCP share the selected workspace. The desktop launcher connects MCP to the running local API.
- For an existing 0.5.0 beta installation, stop its service, then use `sh install-desktop.sh install v0.0.1-beta`. This explicit numbering reset retains the selected workspace. Subsequent upgrades use `update`.

Apple Silicon validation is deferred until compatible hardware is available and is not a prerequisite for this Intel release. Publication requires the selected platform packages and their native acceptance jobs. Ad-hoc signing is not Developer ID signing or notarization. A prerelease and scoped checks do not imply full product qualification.
