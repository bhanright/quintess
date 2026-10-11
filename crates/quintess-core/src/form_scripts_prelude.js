// The part of Acrobat's JavaScript object model that form scripts use:
// the event object, this.getField and Field objects, app, util, the color
// and display constants, and the AForm built-ins (AFNumber_Format,
// AFSimple_Calculate and the rest) that form designers pick from menus.
//
// form_scripts.rs runs this once per batch of scripts, loads the form's
// fields with __quintess.load, then drives each script through
// __quintess.beginEvent / __quintess.finishEvent. Nothing here can reach
// files, the network or other programs: the engine has no such objects,
// and the Acrobat functions that would (mail, submit, launch, print) do
// nothing.
//
// The built-ins follow Acrobat's AForm.js: the same arguments, the same
// separator styles ("1,234.56", "1234.56", "1.234,56", "1234,56",
// "1'234.56") and the same messages, so a form shows and rejects what it
// would in Acrobat.

var __quintess = {
    // One record per terminal field, in document order (see load).
    fields: [],
    indexByName: {},
    alerts: [],
    // Document-level scripts run once to define functions; values they set
    // on fields while doing so are ignored (Acrobat runs them on opening,
    // before anyone has typed anything).
    loadingDocumentScripts: false,
    // What app.platform reports: "WIN" on Windows, "UNIX" elsewhere.
    platform: "WIN"
};

// ---------------------------------------------------------------------------
// Messages, worded as Acrobat words them.

var IDS_GREATER_THAN = "Invalid value: must be greater than or equal to %s.";
var IDS_GT_AND_LT = "Invalid value: must be greater than or equal to %s and less than or equal to %s.";
var IDS_LESS_THAN = "Invalid value: must be less than or equal to %s.";
var IDS_INVALID_MONTH = "** Invalid **";
var IDS_INVALID_DATE = "Invalid date/time: please ensure that the date/time exists. Field";
var IDS_INVALID_DATE2 = " should match format ";
var IDS_INVALID_VALUE = "The value entered does not match the format of the field";
var IDS_AM = "am";
var IDS_PM = "pm";
var IDS_MONTH_NAMES = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
var IDS_DAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

// ---------------------------------------------------------------------------
// Constants scripts compare against.

var color = {
    transparent: ["T"],
    black: ["G", 0],
    white: ["G", 1],
    red: ["RGB", 1, 0, 0],
    green: ["RGB", 0, 1, 0],
    blue: ["RGB", 0, 0, 1],
    cyan: ["CMYK", 1, 0, 0, 0],
    magenta: ["CMYK", 0, 1, 0, 0],
    yellow: ["CMYK", 0, 0, 1, 0],
    dkGray: ["G", 0.25],
    gray: ["G", 0.5],
    ltGray: ["G", 0.75],
    equal: function (first, second) {
        var a = color.convert(first, "RGB");
        var b = color.convert(second, "RGB");
        if (a.length != b.length) return false;
        for (var i = 0; i < a.length; i++) {
            if (Math.abs(a[i] - b[i]) > 0.0001) return false;
        }
        return true;
    },
    convert: function (value, space) {
        var rgb = __quintess.colorToRgb(value);
        if (rgb === null) return ["T"];
        if (space == "G") return ["G", 0.3 * rgb[0] + 0.59 * rgb[1] + 0.11 * rgb[2]];
        if (space == "CMYK") {
            var black = 1 - Math.max(rgb[0], rgb[1], rgb[2]);
            if (black >= 1) return ["CMYK", 0, 0, 0, 1];
            return ["CMYK", (1 - rgb[0] - black) / (1 - black), (1 - rgb[1] - black) / (1 - black), (1 - rgb[2] - black) / (1 - black), black];
        }
        return ["RGB", rgb[0], rgb[1], rgb[2]];
    }
};

var display = { visible: 0, hidden: 1, noPrint: 2, noView: 3 };
var border = { s: "solid", b: "beveled", d: "dashed", i: "inset", u: "underline" };
var font = {
    Times: "Times-Roman", TimesB: "Times-Bold", TimesI: "Times-Italic", TimesBI: "Times-BoldItalic",
    Helv: "Helvetica", HelvB: "Helvetica-Bold", HelvI: "Helvetica-Oblique", HelvBI: "Helvetica-BoldOblique",
    Cour: "Courier", CourB: "Courier-Bold", CourI: "Courier-Oblique", CourBI: "Courier-BoldOblique",
    Symbol: "Symbol", ZapfD: "ZapfDingbats"
};
var highlight = { n: "none", i: "invert", p: "push", o: "outline" };
var style = { ch: "check", cr: "cross", di: "diamond", ci: "circle", st: "star", sq: "square" };
var position = { textOnly: 0, iconOnly: 1, iconTextV: 2, textIconV: 3, iconTextH: 4, textIconH: 5, overlay: 6 };
var scaleHow = { proportional: 0, anamorphic: 1 };
var scaleWhen = { always: 0, never: 1, tooBig: 2, tooSmall: 3 };
var zoomtype = { none: "NoVary", fitP: "FitPage", fitW: "FitWidth", fitH: "FitHeight", fitV: "FitVisibleWidth", pref: "Preferred", refW: "ReflowWidth" };

// Acrobat's `global` object keeps values between scripts; here it lasts
// for one batch and is never written anywhere.
var global = { setPersistent: function () {} };

var console = {
    println: function () {},
    show: function () {},
    hide: function () {},
    clear: function () {}
};

// ---------------------------------------------------------------------------
// app: alerts are collected and shown by Quintess afterwards; nothing waits
// for an answer, opens, mails, launches or schedules anything.

var app = {
    viewerType: "Exchange-Pro",
    viewerVariation: "Full",
    viewerVersion: 21,
    formsVersion: 21,
    language: "ENU",
    get platform() { return __quintess.platform; },
    calculate: true,
    runtimeHighlight: false,
    fs: {},
    alert: function (message) {
        // app.alert takes either the message or an object { cMsg: ... }.
        if (message !== null && typeof message == "object" && "cMsg" in message) message = message.cMsg;
        __quintess.alerts.push(String(message));
        // 1 is the OK button, the answer a person would give to a notice.
        return 1;
    },
    beep: function () {},
    response: function () { return null; },
    setTimeOut: function () { return {}; },
    setInterval: function () { return {}; },
    clearTimeOut: function () {},
    clearInterval: function () {},
    launchURL: function () {},
    mailMsg: function () {},
    openDoc: function () { return null; },
    execMenuItem: function () {},
    goBack: function () {},
    goForward: function () {},
    popUpMenu: function () { return null; },
    popUpMenuEx: function () { return null; },
    thermometer: { begin: function () {}, end: function () {}, value: 0, duration: 0, text: "", cancelled: false }
};

// ---------------------------------------------------------------------------
// The document (`this` in a form script): only what form scripts read.

var numPages = 1;
var pageNum = 0;
var dirty = false;
var calculate = true;
var documentFileName = "";
var path = "";
var info = {};
var numFields = 0;

function getField(name) {
    return __quintess.fieldObject(String(name));
}

function getNthFieldName(index) {
    var record = __quintess.fields[index];
    return record ? record.name : null;
}

// Quintess runs every calculation after each change anyway.
function calculateNow() {}

function resetForm(names) {
    var records = __quintess.fields;
    if (names !== undefined && names !== null) {
        records = [];
        var list = names instanceof Array ? names : [names];
        for (var i = 0; i < list.length; i++) {
            var found = __quintess.fieldObject(String(list[i]));
            if (found) records = records.concat(found.__records());
        }
    }
    for (var j = 0; j < records.length; j++) {
        var empty = (records[j].type == "checkbox" || records[j].type == "radiobutton") ? "Off" : "";
        __quintess.store(records[j], empty);
    }
}

function getPageNthWord() { return ""; }
function getPageNumWords() { return 0; }
function print() {}
function mailForm() {}
function mailDoc() {}
function submitForm() {}
function exportAsFDF() {}
function exportAsXFDF() {}
function importAnFDF() {}
function importAnXFDF() {}
function saveAs() {}
function closeDoc() {}
function gotoNamedDest() {}
function syncAnnotScan() {}

// ---------------------------------------------------------------------------
// Fields.

__quintess.load = function (data) {
    __quintess.fields = data.fields;
    __quintess.indexByName = {};
    for (var i = 0; i < data.fields.length; i++) {
        var record = data.fields[i];
        record.index = i;
        record.textColor = null;
        record.fillColor = null;
        record.borderColor = null;
        record.itemsChanged = false;
        __quintess.indexByName[record.name] = i;
    }
    numFields = data.fields.length;
    numPages = data.pageCount;
    __quintess.platform = data.platform;
};

// The value a script sees: text that reads as a number comes back as a
// number, so `qty.value * price.value` and `a.value + b.value` do sums,
// as they do in Acrobat. valueAsString gives the text as stored.
__quintess.scriptValue = function (record) {
    var text = record.value;
    if (record.type == "text" || record.type == "combobox") {
        var trimmed = text.replace(/^\s+|\s+$/g, "");
        if (trimmed !== "" && /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(trimmed)) return Number(trimmed);
    }
    return text;
};

// Stores what a script assigned, as the text the PDF will hold.
__quintess.store = function (record, value) {
    if (__quintess.loadingDocumentScripts) return;
    if (value === null || value === undefined) value = "";
    if (value instanceof Array) value = value.length ? value[0] : "";
    var text = String(value);
    if (record.type == "checkbox" || record.type == "radiobutton") {
        if (text === "") text = "Off";
    }
    record.value = text;
};

__quintess.fieldObject = function (name) {
    var index = __quintess.indexByName[name];
    if (index !== undefined) return new Field([__quintess.fields[index]], name);
    // A parent name ("income" for "income.1", "income.2") stands for all
    // the fields under it, as Acrobat's field hierarchy does.
    var children = [];
    var prefix = name + ".";
    for (var i = 0; i < __quintess.fields.length; i++) {
        if (__quintess.fields[i].name.indexOf(prefix) == 0) children.push(__quintess.fields[i]);
    }
    return children.length ? new Field(children, name) : null;
};

function Field(records, name) {
    this.__fieldRecords = records;
    this.__fieldName = name;
}

Field.prototype.__records = function () { return this.__fieldRecords; };
Field.prototype.__first = function () { return this.__fieldRecords[0]; };

Field.prototype.getArray = function () {
    var parts = [];
    for (var i = 0; i < this.__fieldRecords.length; i++) {
        parts.push(new Field([this.__fieldRecords[i]], this.__fieldRecords[i].name));
    }
    return parts;
};

(function () {
    function property(name, getter, setter) {
        Object.defineProperty(Field.prototype, name, { get: getter, set: setter || function () {}, configurable: true });
    }
    // Setting a property on a parent field sets it on every field under it.
    function setAll(field, change) {
        for (var i = 0; i < field.__fieldRecords.length; i++) change(field.__fieldRecords[i]);
    }

    property("name", function () { return this.__fieldName; });
    property("type", function () { return this.__first().type; });
    property("value",
        function () { return __quintess.scriptValue(this.__first()); },
        function (value) { setAll(this, function (record) { __quintess.store(record, value); }); });
    property("valueAsString", function () { return this.__first().value; });
    property("defaultValue", function () { return this.__first().defaultValue; });
    property("readonly",
        function () { return this.__first().readonly; },
        function (value) { setAll(this, function (record) { record.readonly = !!value; }); });
    property("required",
        function () { return this.__first().required; },
        function (value) { setAll(this, function (record) { record.required = !!value; }); });
    property("display",
        function () { return this.__first().display; },
        function (value) {
            var number = Number(value);
            if (number >= 0 && number <= 3) setAll(this, function (record) { record.display = Math.floor(number); });
        });
    property("hidden",
        function () { return this.__first().display == display.hidden; },
        function (value) { setAll(this, function (record) { record.display = value ? display.hidden : display.visible; }); });
    property("textColor",
        function () { return this.__first().textColor || color.black; },
        function (value) { setAll(this, function (record) { record.textColor = value; }); });
    property("fgColor",
        function () { return this.__first().textColor || color.black; },
        function (value) { setAll(this, function (record) { record.textColor = value; }); });
    property("fillColor",
        function () { return this.__first().fillColor || color.transparent; },
        function (value) { setAll(this, function (record) { record.fillColor = value; }); });
    property("bgColor",
        function () { return this.__first().fillColor || color.transparent; },
        function (value) { setAll(this, function (record) { record.fillColor = value; }); });
    property("borderColor",
        function () { return this.__first().borderColor || color.transparent; },
        function (value) { setAll(this, function (record) { record.borderColor = value; }); });
    property("strokeColor",
        function () { return this.__first().borderColor || color.transparent; },
        function (value) { setAll(this, function (record) { record.borderColor = value; }); });
    property("multiline", function () { return this.__first().multiline; });
    property("charLimit", function () { return this.__first().charLimit; });
    property("userName", function () { return this.__first().tooltip; });
    property("page", function () { return this.__first().page; });
    property("rect", function () { return this.__first().rect.slice(); });
    property("numItems", function () { return this.__first().items.length; });
    property("exportValues", function () { return this.__first().exportValues.slice(); });
    property("editable", function () { return this.__first().editable; });
    property("doc", function () { return globalThis; });
    property("currentValueIndices",
        function () {
            var record = this.__first();
            if (record.type == "checkbox" || record.type == "radiobutton") return record.exportValues.indexOf(record.value);
            for (var i = 0; i < record.items.length; i++) {
                if (record.items[i][1] == record.value || record.items[i][0] == record.value) return i;
            }
            return -1;
        },
        function (indices) {
            var index = indices instanceof Array ? indices[0] : indices;
            setAll(this, function (record) {
                var choices = (record.type == "checkbox" || record.type == "radiobutton") ? record.exportValues : record.items.map(function (item) { return item[1]; });
                if (index >= 0 && index < choices.length) __quintess.store(record, choices[index]);
            });
        });
    // Properties scripts set for looks; kept for the script to read back.
    var lookProperties = ["alignment", "borderStyle", "lineWidth", "textFont", "textSize", "comb", "doNotScroll", "doNotSpellCheck", "fileSelect", "password", "richText", "rotation", "print", "style", "buttonAlignX", "buttonAlignY", "buttonPosition", "buttonScaleHow", "buttonScaleWhen", "highlight", "commitOnSelChange", "radiosInUnison", "submitName", "delay"];
    lookProperties.forEach(function (name) {
        property(name,
            function () { var looks = this.__first().looks || {}; return looks[name]; },
            function (value) { setAll(this, function (record) { record.looks = record.looks || {}; record.looks[name] = value; }); });
    });
})();

// List and combo box items are [display text, export value] pairs.
Field.prototype.getItemAt = function (index, exportValue) {
    var item = this.__first().items[index];
    if (!item) return null;
    return (exportValue === false) ? item[0] : item[1];
};

Field.prototype.setItems = function (items) {
    var pairs = [];
    for (var i = 0; i < items.length; i++) {
        var item = items[i];
        if (item instanceof Array) pairs.push([String(item[0]), String(item.length > 1 ? item[1] : item[0])]);
        else pairs.push([String(item), String(item)]);
    }
    this.__fieldRecords.forEach(function (record) { record.items = pairs.slice(); record.itemsChanged = true; });
};

Field.prototype.insertItemAt = function (displayText, exportValue, index) {
    if (typeof displayText == "object" && displayText !== null) {
        exportValue = displayText.cExport;
        index = displayText.nIdx;
        displayText = displayText.cName;
    }
    var pair = [String(displayText), String(exportValue === undefined ? displayText : exportValue)];
    this.__fieldRecords.forEach(function (record) {
        var at = (index === undefined || index < 0 || index > record.items.length) ? 0 : index;
        record.items.splice(at, 0, pair);
        record.itemsChanged = true;
    });
};

Field.prototype.deleteItemAt = function (index) {
    this.__fieldRecords.forEach(function (record) {
        var at = (index === undefined) ? record.items.length - 1 : index;
        if (at >= 0 && at < record.items.length) record.items.splice(at, 1);
        record.itemsChanged = true;
    });
};

Field.prototype.clearItems = function () {
    this.__fieldRecords.forEach(function (record) { record.items = []; record.itemsChanged = true; });
};

Field.prototype.isBoxChecked = function (index) {
    var record = this.__first();
    var exportValue = record.exportValues[index || 0];
    return exportValue !== undefined && record.value == exportValue;
};

Field.prototype.isDefaultChecked = function () { return false; };

Field.prototype.checkThisBox = function (index, checked) {
    if (checked === undefined) checked = true;
    this.__fieldRecords.forEach(function (record) {
        var exportValue = record.exportValues[index || 0];
        if (exportValue === undefined) return;
        if (checked) __quintess.store(record, exportValue);
        else if (record.value == exportValue) __quintess.store(record, "Off");
    });
};

Field.prototype.defaultIsChecked = function () {};
Field.prototype.setFocus = function () {};
Field.prototype.setAction = function () {};
Field.prototype.setLock = function () { return false; };
Field.prototype.getLock = function () { return null; };
Field.prototype.buttonSetCaption = function () {};
Field.prototype.buttonGetCaption = function () { return ""; };
Field.prototype.buttonImportIcon = function () { return 0; };
Field.prototype.buttonSetIcon = function () {};
Field.prototype.browseForFileToSubmit = function () {};
Field.prototype.signatureSign = function () { return false; };
Field.prototype.signatureValidate = function () { return 0; };

// ---------------------------------------------------------------------------
// Events. Quintess calls beginEvent, runs the field's script, then reads the
// outcome with finishEvent.

var event = null;

__quintess.beginEvent = function (details) {
    var record = __quintess.fields[details.target];
    var target = new Field([record], record.name);
    var value = details.value;
    if (value === undefined) value = details.name == "Calculate" ? __quintess.scriptValue(record) : record.value;
    event = {
        name: details.name,
        type: "Field",
        target: target,
        targetName: record.name,
        source: details.source === null || details.source === undefined ? null : new Field([__quintess.fields[details.source]], __quintess.fields[details.source].name),
        value: value,
        change: details.change || "",
        changeEx: details.change || "",
        selStart: details.selStart === undefined ? -1 : details.selStart,
        selEnd: details.selEnd === undefined ? -1 : details.selEnd,
        willCommit: !!details.willCommit,
        commitKey: details.willCommit ? 2 : 0,
        keyDown: false,
        modifier: false,
        shift: false,
        fieldFull: false,
        silenceErrors: false,
        rc: true
    };
    __quintess.valueBeforeEvent = value;
    // Each field's text color is worked out afresh by its format script.
    if (details.name == "Format") record.textColor = null;
};

__quintess.finishEvent = function () {
    var value = event.value;
    if (value === null || value === undefined) value = "";
    var outcome = {
        accepted: event.rc !== false,
        value: String(value),
        // A calculation that leaves event.value alone changes nothing (its
        // text isn't rewritten from the number it was read as).
        valueChanged: event.value !== __quintess.valueBeforeEvent,
        change: String(event.change)
    };
    event = null;
    return JSON.stringify(outcome);
};

__quintess.takeAlerts = function () {
    var alerts = __quintess.alerts;
    __quintess.alerts = [];
    return JSON.stringify(alerts);
};

__quintess.colorToRgb = function (value) {
    if (!(value instanceof Array) || value.length == 0) return null;
    var clamp = function (number) { number = Number(number); return isNaN(number) ? 0 : Math.max(0, Math.min(1, number)); };
    switch (value[0]) {
        case "G": return [clamp(value[1]), clamp(value[1]), clamp(value[1])];
        case "RGB": return [clamp(value[1]), clamp(value[2]), clamp(value[3])];
        case "CMYK": {
            var black = clamp(value[4]);
            return [(1 - clamp(value[1])) * (1 - black), (1 - clamp(value[2])) * (1 - black), (1 - clamp(value[3])) * (1 - black)];
        }
        default: return null;
    }
};

__quintess.exportState = function () {
    return JSON.stringify(__quintess.fields.map(function (record) {
        return {
            value: record.value,
            display: record.display,
            readonly: record.readonly,
            required: record.required,
            textColor: __quintess.colorToRgb(record.textColor),
            items: record.itemsChanged ? record.items : null
        };
    }));
};

// ---------------------------------------------------------------------------
// util.

var util = {};

// Groups the digits of a whole number with Acrobat's separator styles:
// 0 "1,234.56", 1 "1234.56", 2 "1.234,56", 3 "1234,56", 4 "1'234.56".
__quintess.separators = function (style) {
    switch (Number(style)) {
        case 1: return { thousands: "", decimal: "." };
        case 2: return { thousands: ".", decimal: "," };
        case 3: return { thousands: "", decimal: "," };
        case 4: return { thousands: "'", decimal: "." };
        default: return { thousands: ",", decimal: "." };
    }
};

__quintess.groupDigits = function (digits, thousands) {
    if (!thousands) return digits;
    var grouped = "";
    for (var i = 0; i < digits.length; i++) {
        if (i > 0 && (digits.length - i) % 3 == 0) grouped += thousands;
        grouped += digits.charAt(i);
    }
    return grouped;
};

// util.printf("%,0.2f", 1234.5) gives "1,234.50". Supports the flags
// ",N" (separator style N), "+", " ", "0" and "#", a width, a precision,
// and the conversions d, f, s and x.
util.printf = function (format) {
    var args = Array.prototype.slice.call(arguments, 1);
    var next = 0;
    return String(format).replace(/%(,[0-4])?([+ 0#]*)(\d+)?(?:\.(\d+))?([dfsx%])/g, function (whole, separatorFlag, flags, width, precision, conversion) {
        if (conversion == "%") return "%";
        var argument = args[next++];
        var text;
        var separatorStyle = separatorFlag ? Number(separatorFlag.charAt(1)) : 1;
        var marks = __quintess.separators(separatorStyle);
        if (conversion == "s") {
            text = String(argument);
        } else if (conversion == "x") {
            text = Math.trunc(Number(argument)).toString(16);
        } else {
            var number = Number(argument);
            if (isNaN(number)) number = 0;
            var digits = conversion == "d" ? 0 : (precision === undefined ? 6 : Number(precision));
            var fixed = Math.abs(conversion == "d" ? Math.trunc(number) : number).toFixed(digits);
            var parts = fixed.split(".");
            text = __quintess.groupDigits(parts[0], marks.thousands);
            if (parts.length > 1) text += marks.decimal + parts[1];
            else if (flags.indexOf("#") >= 0 && conversion == "f") text += marks.decimal;
            var negative = number < 0 && Number(fixed) != 0;
            var sign = negative ? "-" : (flags.indexOf("+") >= 0 ? "+" : (flags.indexOf(" ") >= 0 ? " " : ""));
            if (width && flags.indexOf("0") >= 0) {
                while (sign.length + text.length < Number(width)) text = "0" + text;
            }
            text = sign + text;
        }
        if (width) {
            while (text.length < Number(width)) text = " " + text;
        }
        return text;
    });
};

__quintess.pad = function (number, length) {
    var text = String(number);
    while (text.length < length) text = "0" + text;
    return text;
};

// The date and time patterns util.printd and util.scand understand.
__quintess.dateTokens = ["mmmm", "mmm", "mm", "m", "dddd", "ddd", "dd", "d", "yyyy", "yy", "HH", "H", "hh", "h", "MM", "M", "ss", "s", "tt", "t"];

__quintess.tokenizeDateFormat = function (format) {
    var pieces = [];
    var i = 0;
    while (i < format.length) {
        if (format.charAt(i) == "\\" && i + 1 < format.length) {
            pieces.push({ literal: format.charAt(i + 1) });
            i += 2;
            continue;
        }
        var token = null;
        for (var t = 0; t < __quintess.dateTokens.length; t++) {
            if (format.substr(i, __quintess.dateTokens[t].length) == __quintess.dateTokens[t]) { token = __quintess.dateTokens[t]; break; }
        }
        if (token) { pieces.push({ token: token }); i += token.length; }
        else { pieces.push({ literal: format.charAt(i) }); i++; }
    }
    return pieces;
};

util.printd = function (format, date) {
    if (!(date instanceof Date) || isNaN(date.getTime())) return "";
    if (format === 0) format = "D:yyyymmddHHMMss";
    else if (format === 1) format = "yyyy.mm.dd HH:MM:ss";
    else if (format === 2) format = "m/d/yy h:MM:ss tt";
    var hours = date.getHours();
    var hour12 = hours % 12 == 0 ? 12 : hours % 12;
    return __quintess.tokenizeDateFormat(String(format)).map(function (piece) {
        if (piece.literal !== undefined) return piece.literal;
        switch (piece.token) {
            case "mmmm": return IDS_MONTH_NAMES[date.getMonth()];
            case "mmm": return IDS_MONTH_NAMES[date.getMonth()].substr(0, 3);
            case "mm": return __quintess.pad(date.getMonth() + 1, 2);
            case "m": return String(date.getMonth() + 1);
            case "dddd": return IDS_DAY_NAMES[date.getDay()];
            case "ddd": return IDS_DAY_NAMES[date.getDay()].substr(0, 3);
            case "dd": return __quintess.pad(date.getDate(), 2);
            case "d": return String(date.getDate());
            case "yyyy": return __quintess.pad(date.getFullYear(), 4);
            case "yy": return __quintess.pad(date.getFullYear() % 100, 2);
            case "HH": return __quintess.pad(hours, 2);
            case "H": return String(hours);
            case "hh": return __quintess.pad(hour12, 2);
            case "h": return String(hour12);
            case "MM": return __quintess.pad(date.getMinutes(), 2);
            case "M": return String(date.getMinutes());
            case "ss": return __quintess.pad(date.getSeconds(), 2);
            case "s": return String(date.getSeconds());
            case "tt": return hours < 12 ? IDS_AM : IDS_PM;
            case "t": return (hours < 12 ? IDS_AM : IDS_PM).charAt(0);
        }
        return "";
    }).join("");
};

__quintess.monthFromName = function (word) {
    var lower = word.toLowerCase();
    if (lower.length < 3) return -1;
    if (lower == "sept") return 8;
    for (var i = 0; i < IDS_MONTH_NAMES.length; i++) {
        var name = IDS_MONTH_NAMES[i].toLowerCase();
        if (name == lower || name.substr(0, 3) == lower) return i;
    }
    return -1;
};

// Two-digit years below 50 are in this century, as Acrobat reads them.
__quintess.fullYear = function (year, digitCount) {
    if (digitCount <= 2) return year < 50 ? 2000 + year : 1900 + year;
    return year;
};

// Builds a date, or null when the day doesn't exist (February 30th).
__quintess.makeDate = function (year, monthIndex, day, hours, minutes, seconds) {
    if (monthIndex < 0 || monthIndex > 11 || day < 1 || day > 31 || hours > 23 || minutes > 59 || seconds > 59) return null;
    var date = new Date(year, monthIndex, day, hours, minutes, seconds);
    date.setFullYear(year);
    if (date.getMonth() != monthIndex || date.getDate() != day) return null;
    return date;
};

// util.scand("mm/dd/yyyy", "03/04/2026") reads text written in a pattern.
util.scand = function (format, text) {
    if (format === 0) format = "D:yyyymmddHHMMss";
    else if (format === 1) format = "yyyy.mm.dd HH:MM:ss";
    else if (format === 2) format = "m/d/yy h:MM:ss tt";
    text = String(text);
    var pieces = __quintess.tokenizeDateFormat(String(format));
    var now = new Date();
    var year = now.getFullYear(), month = now.getMonth(), day = now.getDate();
    var hours = 0, minutes = 0, seconds = 0, afternoon = null;
    var at = 0;
    function skipSpaces() { while (at < text.length && /\s/.test(text.charAt(at))) at++; }
    function readNumber(maxDigits) {
        skipSpaces();
        var start = at;
        while (at < text.length && at - start < maxDigits && /\d/.test(text.charAt(at))) at++;
        if (at == start) return null;
        return { value: Number(text.substring(start, at)), digits: at - start };
    }
    function readWord() {
        skipSpaces();
        var start = at;
        while (at < text.length && /[A-Za-zÀ-ɏ]/.test(text.charAt(at))) at++;
        return text.substring(start, at);
    }
    for (var i = 0; i < pieces.length; i++) {
        var piece = pieces[i];
        if (piece.literal !== undefined) {
            if (/\s/.test(piece.literal)) { skipSpaces(); continue; }
            skipSpaces();
            if (text.charAt(at) == piece.literal) at++;
            else return null;
            continue;
        }
        var number;
        switch (piece.token) {
            case "mmmm": case "mmm": {
                var monthIndex = __quintess.monthFromName(readWord());
                if (monthIndex < 0) return null;
                month = monthIndex;
                break;
            }
            case "dddd": case "ddd":
                if (!readWord()) return null;
                break;
            case "mm": case "m":
                number = readNumber(2); if (!number) return null; month = number.value - 1; break;
            case "dd": case "d":
                number = readNumber(2); if (!number) return null; day = number.value; break;
            case "yyyy":
                number = readNumber(4); if (!number) return null; year = __quintess.fullYear(number.value, number.digits); break;
            case "yy":
                number = readNumber(2); if (!number) return null; year = __quintess.fullYear(number.value, 2); break;
            case "HH": case "H": case "hh": case "h":
                number = readNumber(2); if (!number) return null; hours = number.value; break;
            case "MM": case "M":
                number = readNumber(2); if (!number) return null; minutes = number.value; break;
            case "ss": case "s":
                number = readNumber(2); if (!number) return null; seconds = number.value; break;
            case "tt": case "t": {
                var marker = readWord().toLowerCase();
                if (marker == "am" || marker == "a") afternoon = false;
                else if (marker == "pm" || marker == "p") afternoon = true;
                else return null;
                break;
            }
        }
    }
    skipSpaces();
    if (at < text.length) return null;
    if (afternoon !== null) {
        if (hours < 1 || hours > 12) return null;
        hours = (hours % 12) + (afternoon ? 12 : 0);
    }
    return __quintess.makeDate(year, month, day, hours, minutes, seconds);
};

// util.printx("(999) 999-9999", "5551234567") gives "(555) 123-4567".
// 9 takes the next digit, A the next letter, X the next letter or digit,
// ? the next character, * the rest; > < = switch to upper, lower or
// unchanged case; \ makes the next pattern character literal.
util.printx = function (format, source) {
    source = (source === null || source === undefined) ? "" : String(source);
    format = String(format);
    var output = "";
    var at = 0;
    var changeCase = function (character) { return character; };
    var isLetter = function (character) { return /[A-Za-z]/.test(character); };
    var isDigit = function (character) { return /[0-9]/.test(character); };
    for (var i = 0; i < format.length; i++) {
        var command = format.charAt(i);
        if (command == "\\") {
            i++;
            if (i < format.length) output += format.charAt(i);
            continue;
        }
        if (at >= source.length) break;
        switch (command) {
            case "?": output += changeCase(source.charAt(at++)); break;
            case "X":
                while (at < source.length) { var x = source.charAt(at++); if (isLetter(x) || isDigit(x)) { output += changeCase(x); break; } }
                break;
            case "A":
                while (at < source.length) { var a = source.charAt(at++); if (isLetter(a)) { output += changeCase(a); break; } }
                break;
            case "9":
                while (at < source.length) { var nine = source.charAt(at++); if (isDigit(nine)) { output += nine; break; } }
                break;
            case "*":
                while (at < source.length) output += changeCase(source.charAt(at++));
                break;
            case ">": changeCase = function (character) { return character.toUpperCase(); }; break;
            case "<": changeCase = function (character) { return character.toLowerCase(); }; break;
            case "=": changeCase = function (character) { return character; }; break;
            default: output += command;
        }
    }
    return output;
};

util.byteToChar = function (code) { return String.fromCharCode(code); };
util.crackURL = function () { return {}; };
util.iconStreamFromIcon = function () { return null; };
util.streamFromString = function () { return null; };
util.stringFromStream = function () { return ""; };

// ---------------------------------------------------------------------------
// AForm built-ins.

function AFMakeNumber(value) {
    if (typeof value == "number") return value;
    if (typeof value != "string") return null;
    // A comma may stand for the decimal point, as in "1234,5".
    var text = value.replace(/^\s+|\s+$/g, "").replace(",", ".");
    if (!/^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(text)) return null;
    return Number(text);
}

function AFMakeArrayFromList(list) {
    if (typeof list == "string") {
        return list.split(",").map(function (name) { return name.replace(/^\s+|\s+$/g, ""); }).filter(function (name) { return name !== ""; });
    }
    return list;
}

function AFExtractNums(text) {
    var found = String(text).match(/\d+/g);
    return found ? found : null;
}

// True when the whole text matches one of the patterns.
function AFExactMatch(patterns, text) {
    if (!(patterns instanceof Array)) patterns = [patterns];
    for (var i = 0; i < patterns.length; i++) {
        var source = patterns[i] instanceof RegExp ? patterns[i].source : String(patterns[i]);
        if (new RegExp("^(?:" + source + ")$").test(text)) return i + 1;
    }
    return 0;
}

// What the field will hold if this keystroke goes in: the current text
// with the selection replaced by the change.
function AFMergeChange(currentEvent) {
    var value = String(currentEvent.value);
    if (currentEvent.willCommit) return value;
    var prefix = currentEvent.selStart >= 0 ? value.substring(0, currentEvent.selStart) : "";
    var suffix = (currentEvent.selEnd >= 0 && currentEvent.selEnd <= value.length) ? value.substring(currentEvent.selEnd, value.length) : "";
    return prefix + currentEvent.change + suffix;
}

__quintess.invalidValueMessage = function () {
    return IDS_INVALID_VALUE + (event.target ? " [ " + event.target.name + " ]" : "");
};

__quintess.refuse = function (message) {
    if (event.willCommit && !event.silenceErrors) app.alert(message);
    else app.beep(0);
    event.rc = false;
};

function AFSimple(operation, first, second) {
    switch (operation) {
        case "PRD": return first * second;
        case "MIN": return Math.min(first, second);
        case "MAX": return Math.max(first, second);
        default: return first + second;
    }
}

// Totals the named fields (a list or a comma-separated string): SUM, PRD
// (product), AVG, MIN or MAX. Empty fields count as zero.
function AFSimple_Calculate(operation, names) {
    var values = [];
    var list = AFMakeArrayFromList(names);
    for (var i = 0; i < list.length; i++) {
        var field = getField(list[i]);
        if (!field) continue;
        var parts = field.getArray();
        for (var j = 0; j < parts.length; j++) {
            var number = AFMakeNumber(parts[j].value);
            values.push(number === null ? 0 : number);
        }
    }
    if (values.length == 0) { event.value = 0; return; }
    var result = values[0];
    for (var k = 1; k < values.length; k++) result = AFSimple(operation, result, values[k]);
    if (operation == "AVG") result = result / values.length;
    // Rounding to a millionth hides binary floating-point noise (0.1 + 0.2).
    event.value = Math.round(result * 1e6) / 1e6;
}

// Shows a number with nDec decimals, a separator style (see util.printf),
// a negative style (0 minus sign, 1 red, 2 parentheses, 3 red and
// parentheses) and a currency symbol before or after it.
function AFNumber_Format(decimals, separatorStyle, negativeStyle, currencyStyle, currency, currencyFirst) {
    var value = AFMakeNumber(event.value);
    if (value === null) { event.value = ""; return; }
    var negative = value < 0;
    var inParentheses = (negativeStyle == 2 || negativeStyle == 3) && negative;
    currency = currency === undefined || currency === null ? "" : String(currency);
    if (negativeStyle != 0 || currencyFirst) value = Math.abs(value);
    if (negativeStyle == 1 || negativeStyle == 3) event.target.textColor = negative ? color.red : color.black;
    var digits = util.printf("%," + separatorStyle + "." + decimals + "f", value);
    var text = (inParentheses ? "(" : "") + (currencyFirst ? currency : "") + digits + (currencyFirst ? "" : currency) + (inParentheses ? ")" : "");
    // The minus sign goes in front of a leading currency symbol ("-$1.50").
    if (negative && currencyFirst && negativeStyle == 0) text = "-" + text;
    event.value = text;
}

function AFNumber_Keystroke(decimals, separatorStyle, negativeStyle, currencyStyle, currency, currencyFirst) {
    var value = AFMergeChange(event);
    if (!value) return;
    value = value.replace(/^\s+|\s+$/g, "");
    var commaDecimal = separatorStyle == 2 || separatorStyle == 3;
    var pattern;
    if (event.willCommit) pattern = commaDecimal ? /^[+-]?(\d+(,\d*)?|,\d+)$/ : /^[+-]?(\d+(\.\d*)?|\.\d+)$/;
    else pattern = commaDecimal ? /^[+-]?\d*,?\d*$/ : /^[+-]?\d*\.?\d*$/;
    if (!pattern.test(value)) {
        __quintess.refuse(__quintess.invalidValueMessage());
        return;
    }
    // A comma decimal is stored with a point, as JavaScript numbers are.
    if (event.willCommit && commaDecimal) event.value = Number(value.replace(",", "."));
}

function AFPercent_Format(decimals, separatorStyle, percentFirst) {
    var value = AFMakeNumber(event.value);
    if (value === null) { event.value = ""; return; }
    var text = util.printf("%," + separatorStyle + "." + decimals + "f", value * 100);
    event.value = percentFirst ? "%" + text : text + "%";
}

function AFPercent_Keystroke(decimals, separatorStyle) {
    AFNumber_Keystroke(decimals, separatorStyle, 0, 0, "", true);
}

var __AFDateFormats = ["m/d", "m/d/yy", "mm/dd/yy", "mm/yy", "d-mmm", "d-mmm-yy", "dd-mmm-yy", "yy-mm-dd", "mmm-yy", "mmmm-yy", "mmm d, yyyy", "mmmm d, yyyy", "m/d/yy h:MM tt", "m/d/yy HH:MM"];
var __AFTimeFormats = ["HH:MM", "h:MM tt", "HH:MM:ss", "h:MM:ss tt"];

// Reads a date the way Acrobat's AFParseDateEx does: first exactly in the
// field's pattern, then leniently, taking the numbers in the pattern's
// order of month, day and year (or year first when it has four digits),
// accepting month names, and an optional time.
function AFParseDateEx(text, format) {
    text = String(text);
    format = String(format);
    var exact = util.scand(format, text);
    if (exact) return exact;
    var hours = 0, minutes = 0, seconds = 0;
    var time = text.match(/(\d{1,2}):(\d{2})(?::(\d{2}))?\s*(am|pm|a|p)?/i);
    if (time) {
        hours = Number(time[1]);
        minutes = Number(time[2]);
        seconds = time[3] ? Number(time[3]) : 0;
        if (time[4]) {
            if (hours < 1 || hours > 12) return null;
            hours = (hours % 12) + (/^p/i.test(time[4]) ? 12 : 0);
        }
        text = text.replace(time[0], " ");
    }
    var monthIndex = -1;
    var words = text.match(/[A-Za-z]+/g) || [];
    for (var w = 0; w < words.length && monthIndex < 0; w++) monthIndex = __quintess.monthFromName(words[w]);
    var numbers = text.match(/\d+/g) || [];
    // The order in which the pattern names the month, day and year.
    var order = [];
    var dateFormat = format.replace(/\\./g, "");
    for (var i = 0; i < dateFormat.length; i++) {
        var letter = dateFormat.charAt(i);
        if ((letter == "m" || letter == "d" || letter == "y") && order.indexOf(letter) < 0) order.push(letter);
    }
    if (order.length == 0) order = ["m", "d", "y"];
    if (numbers.length >= 3 && numbers[0].length == 4) order = ["y", "m", "d"];
    var now = new Date();
    var parts = { y: null, m: null, d: null };
    if (monthIndex >= 0) {
        parts.m = monthIndex + 1;
        order = order.filter(function (part) { return part != "m"; });
    }
    for (var n = 0; n < numbers.length && n < order.length; n++) parts[order[n]] = { value: Number(numbers[n]), digits: numbers[n].length };
    if (numbers.length > order.length) return null;
    var month = parts.m === null ? null : (typeof parts.m == "number" ? parts.m : parts.m.value);
    if (month === null) return null;
    var day = parts.d === null ? 1 : parts.d.value;
    var year = parts.y === null ? now.getFullYear() : __quintess.fullYear(parts.y.value, parts.y.digits);
    if (parts.d === null && format.indexOf("d") >= 0) return null;
    return __quintess.makeDate(year, month - 1, day, hours, minutes, seconds);
}

function AFDate_FormatEx(format) {
    var value = event.value;
    if (value === "" || value === null || value === undefined) return;
    var date = AFParseDateEx(value, format);
    // A date that can't be read stays as typed rather than vanishing.
    if (date) event.value = util.printd(format, date);
}

function AFDate_Format(formatIndex) {
    AFDate_FormatEx(__AFDateFormats[formatIndex] || __AFDateFormats[0]);
}

function AFDate_KeystrokeEx(format) {
    if (!event.willCommit) return;
    var value = AFMergeChange(event);
    if (!value) return;
    if (!AFParseDateEx(value, format)) {
        __quintess.refuse(IDS_INVALID_DATE + " [ " + event.target.name + " ]" + IDS_INVALID_DATE2 + format);
    }
}

function AFDate_Keystroke(formatIndex) {
    AFDate_KeystrokeEx(__AFDateFormats[formatIndex] || __AFDateFormats[0]);
}

// Reads a time of day such as "2:30 pm", "14:30" or "14:30:15".
function AFParseTime(text) {
    var found = String(text).match(/^\s*(\d{1,2})(?::(\d{1,2}))?(?::(\d{1,2}))?\s*(am|pm|a|p)?\s*$/i);
    if (!found) return null;
    var hours = Number(found[1]);
    var minutes = found[2] ? Number(found[2]) : 0;
    var seconds = found[3] ? Number(found[3]) : 0;
    if (found[4]) {
        if (hours < 1 || hours > 12) return null;
        hours = (hours % 12) + (/^p/i.test(found[4]) ? 12 : 0);
    }
    if (hours > 23 || minutes > 59 || seconds > 59) return null;
    var now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), now.getDate(), hours, minutes, seconds);
}

function AFTime_FormatEx(format) {
    var value = event.value;
    if (value === "" || value === null || value === undefined) return;
    var time = AFParseTime(value) || util.scand(format, value);
    if (time) event.value = util.printd(format, time);
}

function AFTime_Format(formatIndex) {
    AFTime_FormatEx(__AFTimeFormats[formatIndex] || __AFTimeFormats[0]);
}

function AFTime_KeystrokeEx(format) {
    if (!event.willCommit) return;
    var value = AFMergeChange(event);
    if (!value) return;
    if (!AFParseTime(value) && !util.scand(format, value)) __quintess.refuse(__quintess.invalidValueMessage());
}

function AFTime_Keystroke(formatIndex) {
    AFTime_KeystrokeEx(__AFTimeFormats[formatIndex] || __AFTimeFormats[0]);
}

// Zip code (0), zip+4 (1), phone number (2) and Social Security number
// (3): Acrobat's patterns for text being typed and for a finished entry.
var __AFSpecialEntryPatterns = [
    ["\\d{0,5}"],
    ["\\d{0,5}(\\.|[- ])?\\d{0,4}"],
    ["\\d{0,3}(\\.|[- ])?\\d{0,3}(\\.|[- ])?\\d{0,4}", "\\(\\d{0,3}", "\\(\\d{0,3}\\)(\\.|[- ])?\\d{0,3}(\\.|[- ])?\\d{0,4}", "\\(\\d{0,3}(\\.|[- ])?\\d{0,3}(\\.|[- ])?\\d{0,4}", "\\d{0,3}\\)(\\.|[- ])?\\d{0,3}(\\.|[- ])?\\d{0,4}", "011(\\.|[- \\d])*"],
    ["\\d{0,3}(\\.|[- ])?\\d{0,2}(\\.|[- ])?\\d{0,4}"]
];
var __AFSpecialCommitPatterns = [
    ["\\d{5}"],
    ["\\d{5}(\\.|[- ])?\\d{4}"],
    ["\\d{3}(\\.|[- ])?\\d{4}", "\\d{3}(\\.|[- ])?\\d{3}(\\.|[- ])?\\d{4}", "\\(\\d{3}\\)(\\.|[- ])?\\d{3}(\\.|[- ])?\\d{4}", "011(\\.|[- \\d])*"],
    ["\\d{3}(\\.|[- ])?\\d{2}(\\.|[- ])?\\d{4}"]
];

function AFSpecial_Format(kind) {
    var value = event.value;
    if (value === "" || value === null || value === undefined) return;
    var pattern;
    switch (Number(kind)) {
        case 0: pattern = "99999"; break;
        case 1: pattern = "99999-9999"; break;
        case 2: pattern = util.printx("9999999999", value).length >= 10 ? "(999) 999-9999" : "999-9999"; break;
        case 3: pattern = "999-99-9999"; break;
        default: return;
    }
    event.value = util.printx(pattern, value);
}

function AFSpecial_Keystroke(kind) {
    var value = AFMergeChange(event);
    if (!value) return;
    var patterns = (event.willCommit ? __AFSpecialCommitPatterns : __AFSpecialEntryPatterns)[Number(kind)];
    if (!patterns) return;
    if (!AFExactMatch(patterns, value)) __quintess.refuse(__quintess.invalidValueMessage());
}

// An arbitrary mask: 9 a digit, A a letter, O a letter or digit, X any
// character; anything else must appear as written. While typing, the
// mask's literal characters are put in for the person (typing 5551234
// into "999-9999" gives 555-1234).
function AFSpecial_KeystrokeEx(mask) {
    mask = String(mask || "");
    if (!mask) return;
    var fits = function (character, maskCharacter) {
        switch (maskCharacter) {
            case "9": return /[0-9]/.test(character);
            case "A": return /[A-Za-z]/.test(character);
            case "O": return /[A-Za-z0-9]/.test(character);
            case "X": return true;
            default: return character == maskCharacter;
        }
    };
    if (event.willCommit) {
        var value = String(event.value);
        if (!value) return;
        var matches = value.length == mask.length;
        for (var i = 0; matches && i < value.length; i++) matches = fits(value.charAt(i), mask.charAt(i));
        if (!matches) __quintess.refuse(__quintess.invalidValueMessage());
        return;
    }
    var current = String(event.value);
    var start = event.selStart >= 0 ? event.selStart : current.length;
    var end = event.selEnd >= 0 ? event.selEnd : current.length;
    var at = start;
    var change = "";
    for (var c = 0; c < event.change.length; c++) {
        var typed = event.change.charAt(c);
        while (at < mask.length && "9AOX".indexOf(mask.charAt(at)) < 0 && typed != mask.charAt(at)) {
            change += mask.charAt(at);
            at++;
        }
        if (at >= mask.length || !fits(typed, mask.charAt(at))) {
            __quintess.refuse(__quintess.invalidValueMessage());
            return;
        }
        change += typed;
        at++;
    }
    if (current.length - (end - start) + change.length > mask.length) {
        __quintess.refuse(__quintess.invalidValueMessage());
        return;
    }
    event.change = change;
}

// Rejects numbers outside a range. Either bound may be left out.
function AFRange_Validate(checkLower, lower, checkUpper, upper) {
    if (event.value === "") return;
    var message = "";
    if (checkLower && checkUpper) {
        if (event.value < lower || event.value > upper) message = util.printf(IDS_GT_AND_LT, lower, upper);
    } else if (checkLower) {
        if (event.value < lower) message = util.printf(IDS_GREATER_THAN, lower);
    } else if (checkUpper) {
        if (event.value > upper) message = util.printf(IDS_LESS_THAN, upper);
    }
    if (message) {
        if (!event.silenceErrors) app.alert(message, 0);
        event.rc = false;
    }
}
