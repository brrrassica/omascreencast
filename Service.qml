import QtQuick
import Quickshell
import Quickshell.Io

// omascreencast — headless service singleton (`kind: service`).
//
// Owns cast-session state, discovery refreshes, reconciliation, and
// notifications so the workflow survives a closed panel. Skeleton (M0): just
// exposes the plugin id, paths, and a version probe. Real work lands in M1–M6.
Item {
  id: root

  property var shell: null
  property string omarchyPath: Quickshell.env("OMARCHY_PATH")

  readonly property string pluginId: "tech.alatcerdas.omascreencast"
  readonly property string pluginVersion: "0.1.0"

  // Absolute path to this plugin's bundled helper scripts. Derived from the
  // shell config root so helpers are found whether the plugin was cloned by
  // `omarchy plugin add` or dropped in by hand.
  readonly property string scriptsDir: omarchyPath + "/../../.config/omarchy/plugins/" + pluginId + "/scripts"

  // Placeholder dependency probe; replaced by scripts/bootstrap.py in M1.
  Process {
    id: versionProbe
    command: ["python3", "-c", "import sys; print(sys.version.split()[0])"]
  }

  Component.onCompleted: versionProbe.running = true
}
