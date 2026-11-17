import { test } from "node:test";
import assert from "node:assert/strict";
import {
  addressError,
  confirmationText,
  requestAnnouncement,
  canEditConnection,
  countdown,
  deviceNote,
  footerItem,
  portError,
  settingsInput,
  statusLine,
} from "./remote.js";

const base = {
  enabled: false,
  reach: "loopback",
  address: null,
  port: 47390,
  listening: null,
  error: null,
  fingerprint: null,
  device: null,
  connected: false,
  pairing_expires_at: null,
  suggested_address: null,
};

const device = {
  id: "d1",
  name: "Priya's iPhone",
  paired_at: "2026-11-12T10:00:00Z",
  last_seen: "2026-11-14T09:58:00Z",
};

test("nothing is said about remote control while it is off", () => {
  assert.equal(statusLine(base), null);
  assert.equal(footerItem(base), null);
});

test("a listening service names where it listens", () => {
  const on = { ...base, enabled: true, listening: "192.168.1.20:47390" };
  assert.deepEqual(statusLine(on), { tone: "running", word: "Listening", text: "on 192.168.1.20:47390" });
  assert.deepEqual(footerItem(on), { tone: "idle", text: "Remote on" });
});

test("a connected device shows in the footer by name", () => {
  const connected = { ...base, enabled: true, listening: "127.0.0.1:47390", device, connected: true };
  assert.deepEqual(footerItem(connected), { tone: "running", text: "Remote · Priya's iPhone connected" });
});

test("a service that could not start says why, in both places", () => {
  const failed = { ...base, enabled: true, error: "Port 47390 is in use. Choose another port and turn it on again." };
  assert.deepEqual(statusLine(failed), { tone: "failed", word: "Stopped", text: failed.error });
  assert.deepEqual(footerItem(failed), { tone: "failed", text: "Remote stopped: port in use" });
  const other = { ...base, enabled: true, error: "cannot read remote control settings: denied" };
  assert.deepEqual(footerItem(other), { tone: "failed", text: "Remote stopped: see Settings" });
});

test("an enabled service that is not up yet is starting", () => {
  assert.deepEqual(statusLine({ ...base, enabled: true }), { tone: "idle", word: "Starting", text: "" });
});

test("the pairing countdown reads minutes and seconds and stops at zero", () => {
  const now = Date.parse("2026-11-14T10:00:00Z");
  assert.equal(countdown("2026-11-14T10:04:32Z", now), "4:32");
  assert.equal(countdown("2026-11-14T10:05:00Z", now), "5:00");
  assert.equal(countdown("2026-11-14T10:00:09Z", now), "0:09");
  assert.equal(countdown("2026-11-14T09:59:00Z", now), "0:00");
});

test("a device is described by when it paired and when it was last seen", () => {
  const now = Date.parse("2026-11-14T10:00:00Z");
  assert.equal(deviceNote({ ...device, last_seen: "2026-11-14T09:59:30Z" }, now), "Paired 12 Nov · last seen now");
  assert.equal(deviceNote(device, now), "Paired 12 Nov · last seen 2 minutes ago");
  assert.equal(deviceNote({ ...device, last_seen: "2026-11-14T09:59:00Z" }, now), "Paired 12 Nov · last seen 1 minute ago");
  assert.equal(deviceNote({ ...device, last_seen: "2026-11-14T08:00:00Z" }, now), "Paired 12 Nov · last seen 2 hours ago");
  assert.equal(deviceNote({ ...device, last_seen: "2026-11-11T10:00:00Z" }, now), "Paired 12 Nov · last seen 3 days ago");
  assert.equal(deviceNote({ ...device, last_seen: null }, now), "Paired 12 Nov · not seen yet");
});

test("ports outside 1024 to 65535 are refused with a reason", () => {
  assert.equal(portError("47390"), null);
  assert.equal(portError("1024"), null);
  assert.equal(portError("65535"), null);
  for (const bad of ["", "80", "1023", "65536", "47a90", "4739.0", "-1"]) {
    assert.match(portError(bad), /port/i, bad);
  }
});

test("a LAN address must be an address; this computer only needs none", () => {
  assert.equal(addressError("", "loopback"), null);
  assert.equal(addressError("192.168.1.20", "lan"), null);
  assert.equal(addressError("fe80::1", "lan"), null);
  for (const bad of ["", "0.0.0.0", "::", "my-laptop.local", "192.168.1", "300.1.1.1"]) {
    assert.match(addressError(bad, "lan"), /address/i, bad);
  }
});

test("the form becomes the command's settings", () => {
  assert.deepEqual(
    settingsInput({ enabled: true, reach: "lan", address: " 192.168.1.20 ", port: "47400" }),
    { enabled: true, reach: "lan", address: "192.168.1.20", port: 47400 },
  );
  assert.deepEqual(
    settingsInput({ enabled: false, reach: "loopback", address: "10.0.0.2", port: "47390" }),
    { enabled: false, reach: "loopback", address: "", port: 47390 },
  );
});

test("the connection can be edited only while remote control is off", () => {
  assert.equal(canEditConnection(base), true);
  assert.equal(canEditConnection({ ...base, enabled: true }), false);
});

test("a LAN address on the internet is refused", () => {
  for (const ok of ["10.0.0.5", "172.16.4.2", "192.168.1.20", "100.64.0.7", "169.254.10.10", "fd12::1", "fe80::1"]) {
    assert.equal(addressError(ok, "lan"), null, ok);
  }
  for (const bad of ["8.8.8.8", "172.32.0.1", "100.128.0.1", "2001:4860:4860::8888"]) {
    assert.match(addressError(bad, "lan"), /internet/i, bad);
  }
});

test("a device waiting to pair shows in the footer, above everything but an error", () => {
  const request = { device_name: "Pixel 8", confirmation: "482913", expires_at: "2026-11-14T10:02:00Z" };
  const waiting = { ...base, enabled: true, listening: "127.0.0.1:47390", pairing_request: request };
  assert.deepEqual(footerItem(waiting), { tone: "waiting", text: "Remote · a device wants to pair" });
  const connected = { ...waiting, device, connected: true };
  assert.deepEqual(footerItem(connected), { tone: "waiting", text: "Remote · a device wants to pair" });
  const failed = { ...waiting, error: "Port 47390 is in use." };
  assert.equal(footerItem(failed).tone, "failed");
});

test("the confirmation number reads as two groups of three", () => {
  assert.equal(confirmationText("482913"), "482 913");
  assert.equal(confirmationText("000042"), "000 042");
});

test("a pairing request is announced with its name and number", () => {
  assert.equal(
    requestAnnouncement({ device_name: "Pixel 8", confirmation: "482913" }),
    "Pixel 8 wants to pair. Check the number 482 913.",
  );
});
