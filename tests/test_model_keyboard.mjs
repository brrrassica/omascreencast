// Keyboard-activation routing regression tests (#76, #79, #83).
// Model.js is a QML .pragma library, so evaluate its pure functions in a VM
// with the pragma stripped; no Quickshell or Hyprland session is needed.
import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("../Model.js", import.meta.url), "utf8")
  .replace(/^\.pragma library\s*/, "");
const sandbox = {};
vm.runInNewContext(
  `${source}\n;this.keyboardActivation = keyboardActivation;`,
  sandbox,
);

function decide(overrides = {}) {
  return sandbox.keyboardActivation({
    focusSection: "devices",
    selectedIndex: 0,
    confirmDisconnect: false,
    trialActive: false,
    hasService: true,
    device: { uuid: "tv-1" },
    active: false,
    disconnected: false,
    ...overrides,
  });
}

test("#76: disconnect confirmation is reachable from the cursor", () => {
  assert.equal(decide({ active: true, confirmDisconnect: true }).action, "confirm");
  assert.equal(
    decide({
      focusSection: "confirm", selectedIndex: 0, confirmDisconnect: true,
    }).action,
    "confirm",
  );
  assert.equal(
    decide({
      focusSection: "confirm", selectedIndex: 1, confirmDisconnect: true,
    }).action,
    "cancel",
  );
  assert.equal(decide({ focusSection: "confirm" }).action, "none");
});

test("#79: a dropped active device reconnects instead of asking to disconnect", () => {
  const result = decide({
    active: true, disconnected: true, confirmDisconnect: true,
  });
  assert.equal(result.action, "reconnect");
  assert.deepEqual(result.device, { uuid: "tv-1" });
});

test("#83: trial Keep and Revert are both activatable by keyboard", () => {
  assert.equal(
    decide({ focusSection: "trial", selectedIndex: 0, trialActive: true }).action,
    "keepTrial",
  );
  assert.equal(
    decide({ focusSection: "trial", selectedIndex: 1, trialActive: true }).action,
    "revertTrial",
  );
  assert.equal(
    decide({ focusSection: "trial", selectedIndex: 0, trialActive: false }).action,
    "none",
  );
});

test("plain device rows keep their connect/disconnect behaviour", () => {
  assert.equal(decide().action, "connect");
  assert.equal(decide({ active: true }).action, "requestDisconnect");
  assert.equal(decide({ device: null }).action, "none");
  assert.equal(decide({ hasService: false }).action, "none");
});
