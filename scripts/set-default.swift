// Makes an app the default for file extensions, printing the previous default
// for each so the change can be undone.
//
//   swift scripts/set-default.swift /Applications/Leaf.app txt md json log
//   swift scripts/set-default.swift --show txt md          # just print current defaults
//   swift scripts/set-default.swift /Applications/TextEdit.app txt   # undo for .txt
import AppKit
import UniformTypeIdentifiers

var args = Array(CommandLine.arguments.dropFirst())
guard !args.isEmpty else {
    print("usage: set-default.swift (<App.app> | --show) ext...")
    exit(1)
}
let showOnly = args.first == "--show"
let appURL = showOnly ? nil : URL(fileURLWithPath: args.removeFirst())
if showOnly { args.removeFirst() }

func currentDefault(_ type: UTType) -> String {
    NSWorkspace.shared.urlForApplication(toOpen: type)?.lastPathComponent ?? "(none)"
}

var failed = false
let group = DispatchGroup()
for ext in args {
    // Extensions no app declares get a dynamic type (dyn.…); that works too.
    guard let type = UTType(filenameExtension: ext) else {
        print(".\(ext): no type"); continue
    }
    let before = currentDefault(type)
    guard let appURL else {
        print(".\(ext)\t\(type.identifier)\t\(before)"); continue
    }
    group.enter()
    NSWorkspace.shared.setDefaultApplication(at: appURL, toOpen: type) { error in
        if let error {
            failed = true
            print(".\(ext)\t\(type.identifier)\tFAILED: \(error.localizedDescription)")
        } else {
            print(".\(ext)\t\(type.identifier)\t\(before) → \(appURL.lastPathComponent)")
        }
        group.leave()
    }
}
// The completion handlers run on the main queue; keep it spinning until they finish.
while group.wait(timeout: .now()) == .timedOut {
    RunLoop.main.run(until: Date().addingTimeInterval(0.05))
}
exit(failed ? 1 : 0)
