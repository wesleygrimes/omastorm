import QtQuick
import QtTest 1.2
import Quickshell
import Quickshell.Io

ShellRoot {
    FloatingWindow {
        visible: true
        implicitWidth: 640
        implicitHeight: 480
        Item {
            anchors.fill: parent
            TestEvent { id: events }
            // This clipboard belongs to the offscreen Qt process.
            TextInput { id: donor; visible: false; readOnly: true }
            LocationPicker {
                id: picker
                anchors.fill: parent
                theme: ({background: "#101010", foreground: "#eeeeee", accent: "#88aaff", font: "sans-serif"})
                session: ({engine: {state: {mode: "live"}}, locating: false,
                    locationPending: false, ipLocationDismissed: true, locationError: ""})
                engine: QtObject {
                    signal placesReady(var message)
                    property string lastQuery: ""
                    function send(command) { lastQuery = command.query; }
                }
            }
        }
    }
    IpcHandler {
        target: "clipboardTest"
        function open(query: string): void { picker.show(query); }
        function prompt(): void { picker.show("", true); }
        function close(): void { picker.close(); }
        function closeBeforeFocus(): void { picker.show("late"); picker.close(); }
        function offer(text: string): void { donor.text = text; donor.selectAll(); donor.copy(); }
        function type(text: string): void {
            for (var c of text) events.keyClickChar(c, Qt.NoModifier, -1);
        }
        function key(sequence: string): bool { return events.keySequence(sequence); }
        function middle(): bool {
            return events.mouseClick(picker.testField, 10, picker.testField.height / 2,
                Qt.MiddleButton, Qt.NoModifier, -1);
        }
        function status(): string {
            var field = picker.testField, reader = picker.testReader;
            return JSON.stringify({open: picker.open, focused: picker.fieldFocused,
                canPaste: field.canPaste,
                query: picker.query, searched: picker.engine.lastQuery,
                running: reader.running, error: picker.pasteError,
                collected: reader.stdout.text.length});
        }
    }
}
