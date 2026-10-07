pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The way to take a new screenshot: a mode, and the delay before it is
// taken. Used in the popover of the header and in the empty state.
ColumnLayout {
    id: panel

    property int delay: 0
    // Lay the modes out in a row (the empty state) or a column (the popover).
    property bool horizontal: false

    signal captureRequested(string mode, int delay)
    signal delayEdited(int delay)

    spacing: TelamonStyle.spacingLarge

    readonly property var modes: [
        { mode: "region", text: qsTr("Region"), symbol: Symbols.ScreenshotRegion },
        { mode: "full", text: qsTr("Full Screen"), symbol: Symbols.Fullscreen },
        { mode: "active-window", text: qsTr("Active Window"), symbol: Symbols.WebAsset },
        { mode: "window", text: qsTr("Window"), symbol: Symbols.SelectWindow },
        { mode: "screen", text: qsTr("Current Screen"), symbol: Symbols.ScreenshotMonitor }
    ]

    GridLayout {
        Layout.alignment: Qt.AlignHCenter
        Layout.fillWidth: !panel.horizontal
        columns: panel.horizontal ? panel.modes.length : 1
        rowSpacing: TelamonStyle.spacingSmall
        columnSpacing: TelamonStyle.spacingSmall

        Repeater {
            model: panel.modes
            TelamonButton {
                required property var modelData
                Layout.fillWidth: !panel.horizontal
                text: modelData.text
                symbol: modelData.symbol
                onClicked: panel.captureRequested(modelData.mode, panel.delay)
            }
        }
    }

    RowLayout {
        Layout.alignment: Qt.AlignHCenter
        spacing: TelamonStyle.spacing

        TelamonLabel {
            text: qsTr("Delay")
            Layout.alignment: Qt.AlignVCenter
        }
        TelamonSpinBox {
            from: 0
            to: 60
            editable: true
            suffix: qsTr(" s")
            value: panel.delay
            onValueModified: panel.delayEdited(value)
            Accessible.name: qsTr("Delay in seconds")
        }
    }
}
