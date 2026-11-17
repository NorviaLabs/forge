import { test } from "node:test";
import assert from "node:assert/strict";
import {
  approvalHeading,
  compactTarget,
  folderError,
  offHere,
  targetChip,
  targetRequest,
  targetTitle,
} from "./target.js";

const deep = { host: "devbox", path: "/home/me/work/billing/api", ask: true };
const shallow = { host: "devbox", path: "/srv/app", ask: true };

test("a remote target shows its host and the last two folders", () => {
  assert.equal(compactTarget(shallow), "devbox:/srv/app");
  assert.equal(compactTarget(deep), "devbox:…/billing/api");
  assert.equal(compactTarget({ host: "devbox", path: "/srv/app/" }), "devbox:/srv/app");
  assert.equal(compactTarget({ host: "devbox", path: "/" }), "devbox:/");
  assert.equal(compactTarget(null), "");
});

test("the chip says where the session runs, and only the host when narrow", () => {
  assert.equal(targetChip(deep, false), "Runs on devbox:…/billing/api");
  assert.equal(targetChip(deep, true), "devbox");
  assert.equal(targetTitle(deep), "devbox:/home/me/work/billing/api");
  assert.equal(targetChip(null, false), "");
});

test("a remote folder must be an absolute path on the host", () => {
  assert.equal(folderError("", "devbox"), "Enter the folder on devbox.");
  assert.equal(folderError("   ", "devbox"), "Enter the folder on devbox.");
  assert.equal(folderError("srv/app", "devbox"), "Use an absolute path, starting with /.");
  assert.equal(folderError("~/app", "devbox"), "Use an absolute path, starting with /.");
  assert.equal(folderError("/srv/app", "devbox"), "");
  assert.equal(folderError(" /srv/app ", "devbox"), "");
});

test("creating asks for a target only for another machine", () => {
  assert.equal(targetRequest({ runOn: "local", folder: "/x", ask: true }), null);
  assert.deepEqual(targetRequest({ runOn: "devbox", folder: " /srv/app ", ask: true }), {
    host: "devbox",
    path: "/srv/app",
    ask: true,
  });
  assert.deepEqual(targetRequest({ runOn: "devbox", folder: "/srv/app", ask: false }), {
    host: "devbox",
    path: "/srv/app",
    ask: false,
  });
});

test("an approval for the host says so", () => {
  assert.equal(approvalHeading({ target: "devbox", tool: "bash" }), "Run this command on devbox?");
  assert.equal(approvalHeading({ target: null, tool: "bash" }), "Needs your approval");
  assert.equal(approvalHeading({ tool: "bash" }), "Needs your approval");
});

test("the panels that show this Mac say why they are off", () => {
  assert.equal(
    offHere(shallow),
    "This session works in /srv/app on devbox. Files, Changes, Git, GitHub and the terminal show this Mac, so they are off.",
  );
  assert.equal(offHere(null), "");
});
