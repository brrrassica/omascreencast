import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// omascreencast — bar widget entry point and popup host.
//
// Skeleton (M0): shows a placeholder label and toggles the Panel popup
// lifecycle. The display list, Chromecast discovery, and cast controls land in
// M1–M5, and the monitor arranger canvas in M4. The popup body (a PopupCard
// supplied with the injected `bar` + an anchor item) is added then.
Panel {
  id: root
  moduleName: "tech.alatcerdas.omascreencast"
  ipcTarget: "omascreencast"
  manageIpc: false

  // Read from this widget's inline shell.json entry (see Ui/BarWidget.setting).
  readonly property string codec: setting("codec", "auto")
  readonly property int fps: setting("fps", 30)
  readonly property int bitrateMbps: setting("bitrateMbps", 6)
  readonly property string audio: setting("audio", "Off")

  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground

  implicitWidth: label.implicitWidth + 14
  implicitHeight: bar ? bar.barSize : Style.bar.sizeHorizontal

  Text {
    id: label
    anchors.centerIn: parent
    text: "Cast"
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Math.round(root.implicitHeight * 0.5)
  }

  MouseArea {
    anchors.fill: parent
    cursorShape: Qt.PointingHandCursor
    onClicked: root.toggle()
  }
}
