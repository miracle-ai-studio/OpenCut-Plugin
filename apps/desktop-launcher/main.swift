import AppKit
import Foundation
import Metal

private final class OpenCutLauncher: NSObject, NSApplicationDelegate {
    private let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 660, height: 340),
                                  styleMask: [.titled, .closable, .miniaturizable],
                                  backing: .buffered, defer: false)
    private let status = NSTextField(wrappingLabelWithString: "正在检查 OpenCut…")
    private let detail = NSTextField(wrappingLabelWithString: "")
    private let editor = NSButton(title: "打开编辑器", target: nil, action: nil)
    private let retry = NSButton(title: "安装 / 启动", target: nil, action: nil)
    private let check = NSButton(title: "重新检查", target: nil, action: nil)
    private let stop = NSButton(title: "停止服务", target: nil, action: nil)
    private let restart = NSButton(title: "重启服务", target: nil, action: nil)
    private let standard = NSButton(title: "切换标准工作区", target: nil, action: nil)
    private let installApp = NSButton(title: "安装桌面应用", target: nil, action: nil)
    private var busy = false
    private var webURL: URL?

    private var dataDirectory: URL {
        if let value = ProcessInfo.processInfo.environment["OPENCUT_PLUGIN_DATA_DIR"], !value.isEmpty {
            return URL(fileURLWithPath: value, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".opencut-plugin", isDirectory: true)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        window.title = "OpenCut"
        window.center()
        let content = NSView(frame: window.contentView!.bounds)
        window.contentView = content
        let title = NSTextField(labelWithString: "OpenCut")
        title.font = .boldSystemFont(ofSize: 25)
        title.frame = NSRect(x: 28, y: 279, width: 600, height: 35)
        content.addSubview(title)
        status.font = .systemFont(ofSize: 15, weight: .medium)
        status.frame = NSRect(x: 28, y: 227, width: 604, height: 42)
        content.addSubview(status)
        detail.font = .systemFont(ofSize: 12)
        detail.textColor = .secondaryLabelColor
        detail.frame = NSRect(x: 28, y: 141, width: 604, height: 80)
        content.addSubview(detail)
        editor.frame = NSRect(x: 15, y: 30, width: 125, height: 34)
        retry.frame = NSRect(x: 142, y: 30, width: 125, height: 34)
        restart.frame = NSRect(x: 269, y: 30, width: 125, height: 34)
        stop.frame = NSRect(x: 396, y: 30, width: 125, height: 34)
        check.frame = NSRect(x: 523, y: 30, width: 125, height: 34)
        standard.frame = NSRect(x: 18, y: 84, width: 180, height: 34)
        installApp.frame = NSRect(x: 208, y: 84, width: 160, height: 34)
        editor.target = self; editor.action = #selector(openEditor)
        retry.target = self; retry.action = #selector(installAndStart)
        restart.target = self; restart.action = #selector(restartService)
        stop.target = self; stop.action = #selector(stopService)
        check.target = self; check.action = #selector(refresh)
        standard.target = self; standard.action = #selector(switchToStandardWorkspace)
        installApp.target = self; installApp.action = #selector(installApplication)
        [editor, retry, restart, stop, check, standard, installApp].forEach(content.addSubview)
        editor.isEnabled = false
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        if FileManager.default.fileExists(atPath: dataDirectory.appendingPathComponent("install.json").path) {
            startSelectedService()
        } else {
            refresh()
        }
    }

    private func execute(_ executable: String, _ arguments: [String], timeout: TimeInterval = 90) throws -> (Int32, String, String) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        var environment = ProcessInfo.processInfo.environment
        environment["PATH"] = [environment["PATH"] ?? "", "/opt/homebrew/bin", "/usr/local/bin",
                               FileManager.default.homeDirectoryForCurrentUser.path + "/.local/bin",
                               "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin"].joined(separator: ":")
        process.environment = environment
        let output = Pipe(), error = Pipe()
        process.standardOutput = output; process.standardError = error
        try process.run()
        // Drain both streams concurrently so neither pipe can block the child.
        let queue = DispatchQueue(label: "opencut.launcher.output", attributes: .concurrent)
        let group = DispatchGroup()
        var out = Data(), err = Data()
        group.enter(); queue.async { out = output.fileHandleForReading.readDataToEndOfFile(); group.leave() }
        group.enter(); queue.async { err = error.fileHandleForReading.readDataToEndOfFile(); group.leave() }
        let deadline = Date().addingTimeInterval(timeout)
        while process.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.1) }
        if process.isRunning { process.terminate() }
        process.waitUntilExit(); group.wait()
        if Date() >= deadline { throw NSError(domain: "OpenCut", code: 1, userInfo: [NSLocalizedDescriptionKey: "OpenCut 启动超时"]) }
        return (process.terminationStatus, String(decoding: out, as: UTF8.self), String(decoding: err, as: UTF8.self))
    }

    private func installedScript() throws -> (String, String) {
        let config = dataDirectory.appendingPathComponent("install.json")
        let data = try Data(contentsOf: config)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              json["format"] as? String == "opencut.plugin-install.v1",
              let runtime = json["runtime"] as? String, runtime.hasPrefix("/"),
              runtime.hasSuffix("/runtime") else {
            throw NSError(domain: "OpenCut", code: 2, userInfo: [NSLocalizedDescriptionKey: "安装记录无效，请重新安装"]) }
        let root = URL(fileURLWithPath: runtime, isDirectory: true).deletingLastPathComponent()
        let node = root.appendingPathComponent("runtime/bin/libexec/runtimes/node/bin/node").path
        let script = root.appendingPathComponent("plugins/opencut/scripts/web.mjs").path
        guard FileManager.default.isExecutableFile(atPath: node), FileManager.default.fileExists(atPath: script) else {
            throw NSError(domain: "OpenCut", code: 3, userInfo: [NSLocalizedDescriptionKey: "安装文件缺失，请重新安装"]) }
        return (node, script)
    }

    private func service(_ action: String) throws -> [String: Any] {
        let (node, script) = try installedScript()
        let result = try execute(node, [script, action], timeout: 75)
        guard let data = result.1.data(using: .utf8),
              let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let value = json["data"] as? [String: Any], json["error"] is NSNull,
              (result.0 == 0 || action == "status") else {
            throw NSError(domain: "OpenCut", code: 4, userInfo: [NSLocalizedDescriptionKey: result.2.isEmpty ? "服务未能\(action)" : String(result.2.suffix(500))]) }
        return value
    }

    private func metalDescription() -> String {
        if let device = MTLCreateSystemDefaultDevice() { return "Metal 可用：\(device.name)；视频播放仍需实际渲染验证。" }
        return "此进程无法取得 Metal 图形设备；视频预览可能报 CAPABILITY_UNAVAILABLE。"
    }

    private func run(_ operation: @escaping () throws -> [String: Any]) {
        guard !busy else { return }
        busy = true; retry.isEnabled = false; restart.isEnabled = false; stop.isEnabled = false; check.isEnabled = false; standard.isEnabled = false; installApp.isEnabled = false
        status.stringValue = "正在检查服务…"
        DispatchQueue.global(qos: .userInitiated).async {
            let outcome = Result { try operation() }
            DispatchQueue.main.async {
                self.busy = false; self.retry.isEnabled = true; self.restart.isEnabled = true; self.stop.isEnabled = true; self.check.isEnabled = true; self.standard.isEnabled = true; self.installApp.isEnabled = true
                switch outcome {
                case .success(let response):
                    let ready = response["ready"] as? Bool == true
                    let candidate = (response["web_url"] as? String).flatMap(URL.init(string:))
                    self.webURL = ready && candidate?.scheme == "http" && candidate?.host == "127.0.0.1" && candidate?.port != nil ? candidate : nil
                    self.editor.isEnabled = self.webURL != nil
                    self.status.stringValue = ready ? "OpenCut 服务已就绪" : "OpenCut 服务尚未就绪"
                    let note = response["codex_note"] as? String ?? ""
                    self.detail.stringValue = self.metalDescription() + (self.webURL == nil ? "" : "\n\(self.webURL!.absoluteString)") +
                        (note.isEmpty ? "" : "\n" + note)
                case .failure(let error):
                    self.webURL = nil; self.editor.isEnabled = false
                    self.status.stringValue = "OpenCut 需要安装或启动"
                    self.detail.stringValue = error.localizedDescription + "\n" + self.metalDescription()
                }
            }
        }
    }

    @objc private func refresh() { run { try self.service("status") } }
    private func startSelectedService() {
        run {
            let current = try self.service("status")
            if current["ready"] as? Bool == true { return current }
            guard current["state"] as? String == "stopped" else {
                throw NSError(domain: "OpenCut", code: 11, userInfo: [NSLocalizedDescriptionKey: "现有服务状态异常，请检查后使用重启服务"])
            }
            guard MTLCreateSystemDefaultDevice() != nil else {
                throw NSError(domain: "OpenCut", code: 8, userInfo: [NSLocalizedDescriptionKey: "当前桌面会话无法访问 Metal"])
            }
            return try self.service("start")
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        window.makeKeyAndOrderFront(nil)
        startSelectedService()
        return true
    }
    @objc private func stopService() { run { try self.service("stop") } }
    @objc private func restartService() {
        run {
            guard MTLCreateSystemDefaultDevice() != nil else {
                throw NSError(domain: "OpenCut", code: 8, userInfo: [NSLocalizedDescriptionKey: "桌面会话无法访问 Metal，原服务保持运行"])
            }
            let current = try self.service("status")
            if current["ready"] as? Bool == true { _ = try self.service("stop") }
            return try self.service("start")
        }
    }

    @objc private func installAndStart() {
        run {
            guard let resources = Bundle.main.resourceURL else { throw NSError(domain: "OpenCut", code: 5) }
            let archive = resources.appendingPathComponent("opencut-release.zip").path
            let installer = resources.appendingPathComponent("install-desktop.sh").path
            let metadata = try self.execute("/usr/bin/unzip", ["-p", archive, "opencut/release.json"])
            guard metadata.0 == 0, let bytes = metadata.1.data(using: .utf8),
                  let release = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
                  let version = release["version"] as? String else {
                throw NSError(domain: "OpenCut", code: 7, userInfo: [NSLocalizedDescriptionKey: "发行包信息无效"]) }
            let config = self.dataDirectory.appendingPathComponent("install.json")
            let previous = (try? Data(contentsOf: config)).flatMap {
                (try? JSONSerialization.jsonObject(with: $0) as? [String: Any])?["version"] as? String
            }
            // Explicit release numbering reset; installation retains the selected workspace.
            let numberingReset = version == "0.0.1-beta" && (previous?.hasPrefix("0.5.0-beta.") ?? false)
            let mode = previous == nil || previous == version || numberingReset ? "install" : "update"
            let hasCodex = (try? self.execute("/usr/bin/which", ["codex"]).0) == 0
            let args = [installer, mode, "--archive", archive] + (hasCodex ? [] : ["--no-register-codex"])
            let installed = try self.execute("/bin/sh", args, timeout: 300)
            guard installed.0 == 0 else {
                throw NSError(domain: "OpenCut", code: 6, userInfo: [NSLocalizedDescriptionKey: String((installed.2 + installed.1).suffix(700))]) }
            var result = try self.service("start")
            if !hasCodex { result["codex_note"] = "未找到 Codex；服务和编辑器可用，安装 Codex 后再点击安装 / 启动即可注册 MCP。" }
            return result
        }
    }

    @objc private func switchToStandardWorkspace() {
        run {
            guard MTLCreateSystemDefaultDevice() != nil, let resources = Bundle.main.resourceURL else {
                throw NSError(domain: "OpenCut", code: 8, userInfo: [NSLocalizedDescriptionKey: "需要在可使用 Metal 的桌面会话中切换工作区"])
            }
            let (node, _) = try self.installedScript()
            let result = try self.execute(node, [
                resources.appendingPathComponent("switch-workspace.mjs").path,
                resources.appendingPathComponent("opencut-release.zip").path,
                resources.appendingPathComponent("install-desktop.sh").path,
            ], timeout: 1200)
            guard result.0 == 0, let data = result.1.data(using: .utf8),
                  let response = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw NSError(domain: "OpenCut", code: 9, userInfo: [NSLocalizedDescriptionKey: String((result.2 + result.1).suffix(700))])
            }
            return response
        }
    }

    @objc private func installApplication() {
        run {
            let source = Bundle.main.bundleURL
            let applications = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications", isDirectory: true)
            let destination = applications.appendingPathComponent("OpenCut.app", isDirectory: true)
            if source.standardizedFileURL != destination.standardizedFileURL {
                try FileManager.default.createDirectory(at: applications, withIntermediateDirectories: true)
                let replacing = FileManager.default.fileExists(atPath: destination.path)
                if replacing {
                    let attributes = try FileManager.default.attributesOfItem(atPath: destination.path)
                    guard attributes[.type] as? FileAttributeType == .typeDirectory,
                          let installed = Bundle(url: destination),
                          installed.bundleIdentifier == "io.opencut.desktop",
                          let installedVersion = installed.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String,
                          let candidateVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String,
                          candidateVersion.compare(installedVersion, options: .numeric) != .orderedAscending else {
                        throw NSError(domain: "OpenCut", code: 10, userInfo: [NSLocalizedDescriptionKey: "目标不是可更新的 OpenCut 应用，或已安装版本更新"])
                    }
                }
                let staged = applications.appendingPathComponent(".OpenCut-\(UUID().uuidString).app")
                let previous = applications.appendingPathComponent(".OpenCut-replacing-\(UUID().uuidString).app")
                var movedPrevious = false
                do {
                    try FileManager.default.copyItem(at: source, to: staged)
                    let verified = try self.execute("/usr/bin/codesign", ["--verify", "--strict", staged.path])
                    guard verified.0 == 0 else {
                        throw NSError(domain: "OpenCut", code: 12, userInfo: [NSLocalizedDescriptionKey: "新应用校验失败，原安装保留"])
                    }
                    if replacing {
                        try FileManager.default.moveItem(at: destination, to: previous)
                        movedPrevious = true
                    }
                    try FileManager.default.moveItem(at: staged, to: destination)
                } catch {
                    if movedPrevious && !FileManager.default.fileExists(atPath: destination.path) {
                        try FileManager.default.moveItem(at: previous, to: destination)
                    }
                    try? FileManager.default.removeItem(at: staged)
                    throw error
                }
                if movedPrevious { try FileManager.default.removeItem(at: previous) }
            }
            var result = try self.service("status")
            result["codex_note"] = "OpenCut.app 已安装到应用程序目录。"
            return result
        }
    }

    @objc private func openEditor() { if let webURL { NSWorkspace.shared.open(webURL) } }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

let app = NSApplication.shared
private let delegate = OpenCutLauncher()
app.delegate = delegate
app.run()
