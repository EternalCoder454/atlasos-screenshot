pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// A dialog with a path field and the system file chooser behind its Browse
// button: Save As and Open Image.
TelamonDialog {
    id: dialog

    property bool saveMode: false
    property string acceptText: qsTr("OK")
    property var nameFilters: []
    property string placeholder: ""
    property string errorText: ""

    signal chosen(string path)

    function openWith(path) {
        field.path = path;
        dialog.errorText = "";
        dialog.open();
    }

    preferredWidth: Kirigami.Units.gridUnit * 34

    footerContent: [
        SecondaryButton {
            text: qsTr("Cancel")
            onClicked: dialog.close()
        },
        PrimaryButton {
            text: dialog.acceptText
            enabled: field.path.trim().length > 0
            onClicked: dialog.chosen(field.path)
        }
    ]

    TelamonFileField {
        id: field
        Layout.fillWidth: true
        saveMode: dialog.saveMode
        nameFilters: dialog.nameFilters
        placeholderText: dialog.placeholder
        title: dialog.title
        Accessible.name: dialog.saveMode ? qsTr("File name") : qsTr("Image file")
        onEdited: dialog.errorText = ""
    }

    TelamonLabel {
        Layout.fillWidth: true
        visible: dialog.errorText.length > 0
        text: dialog.errorText
        color: TelamonStyle.error
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
    }
}
