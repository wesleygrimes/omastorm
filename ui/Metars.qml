import QtQuick
import "Metar.js" as Metar

// One surface's METAR chips (DESIGN.md, metar chips): the reports its engine
// connection answered for the selected NEXRAD site and the one on the card.
// Cleared when the site changes or METARs stop applying; while on, refreshed
// every ten minutes and just after each hour, when stations report.
QtObject {
    id: metar
    required property var engine
    required property var session
    // The map whose view box the query covers.
    required property var view
    property var metars: []
    property var selected: null
    readonly property bool available: Metar.available(engine.state, engine.site, engine.source)
    readonly property bool shown: session.metarEnabled && available && metars.length > 0
    function toggle() {
        if (available) session.metarEnabled = !session.metarEnabled;
    }
    function clear() {
        metars = [];
        selected = null;
    }
    function request() {
        if (!session.metarEnabled) selected = null;
        else if (!available) clear();
        else engine.send(Metar.command(engine.site, view.viewBbox(), session.config.values));
    }
    readonly property string siteId: engine.selectedSiteId
    // Source, site, and state bindings can update after selectedSiteId.
    // Query once the complete selection is available to the eligibility check.
    onSiteIdChanged: { clear(); Qt.callLater(request); }
    property Connections engineReplies: Connections {
        target: metar.engine
        function onMetarsReady(message) { metar.metars = message.results || []; }
        function onStateChanged() {
            if (!Metar.available(metar.engine.state, metar.engine.site, metar.engine.source)) metar.clear();
        }
    }
    property Connections sessionChanges: Connections {
        target: metar.session
        function onMetarEnabledChanged() {
            if (!metar.session.metarEnabled) metar.selected = null;
            else if (!metar.metars.length) metar.request();
        }
    }
    property Connections configChanges: Connections {
        target: metar.session.config
        function onValuesChanged() { if (metar.session.metarEnabled) metar.request(); }
    }
    property Timer refresh: Timer {
        interval: 600000
        repeat: true
        running: metar.session.metarEnabled && metar.available
        onTriggered: metar.request()
    }
    property Timer topOfHour: Timer {
        interval: 60000
        repeat: true
        running: metar.session.metarEnabled && metar.available
        onTriggered: { if (new Date().getUTCMinutes() <= 1) metar.request(); }
    }
}
