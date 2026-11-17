// Remote control in Settings and the footer: how the service's status reads,
// and what the page checks before it sends new settings. The backend decides
// everything else; these only word it.

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// The line under the connection fields: a state mark and its words, or null
// while remote control is off.
export function statusLine(status) {
  if (!status?.enabled) return null;
  if (status.error) return { tone: "failed", word: "Stopped", text: status.error };
  if (status.listening) return { tone: "running", word: "Listening", text: `on ${status.listening}` };
  return { tone: "idle", word: "Starting", text: "" };
}

// The footer's remote item, or null while remote control is off.
export function footerItem(status) {
  if (!status?.enabled) return null;
  if (status.error) {
    const why = /in use/i.test(status.error) ? "port in use" : "see Settings";
    return { tone: "failed", text: `Remote stopped: ${why}` };
  }
  // A device waiting on your decision outranks everything but an error.
  if (status.pairing_request) return { tone: "waiting", text: "Remote · a device wants to pair" };
  if (status.connected && status.device) {
    return { tone: "running", text: `Remote · ${status.device.name} connected` };
  }
  return { tone: "idle", text: "Remote on" };
}

// "482 913": six digits read as two groups.
export function confirmationText(confirmation) {
  const digits = String(confirmation ?? "");
  return `${digits.slice(0, 3)} ${digits.slice(3)}`;
}

// What a screen reader hears when a device asks to pair.
export function requestAnnouncement(request) {
  return `${request.device_name} wants to pair. Check the number ${confirmationText(request.confirmation)}.`;
}

// "4:32" until the pairing code expires; "0:00" once it has.
export function countdown(expiresAt, now) {
  const left = Math.max(0, Math.floor((Date.parse(expiresAt) - now) / 1000));
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
}

function ago(then, now) {
  const seconds = Math.max(0, Math.floor((now - then) / 1000));
  if (seconds < 60) return "now";
  const unit = (n, word) => `${n} ${word}${n === 1 ? "" : "s"} ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return unit(minutes, "minute");
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return unit(hours, "hour");
  return unit(Math.floor(hours / 24), "day");
}

// "Paired 12 Nov · last seen now", from the device record.
export function deviceNote(device, now) {
  const paired = new Date(device.paired_at);
  const pairedText = `Paired ${paired.getDate()} ${MONTHS[paired.getMonth()]}`;
  const seen = device.last_seen ? `last seen ${ago(Date.parse(device.last_seen), now)}` : "not seen yet";
  return `${pairedText} · ${seen}`;
}

// Why a port the user typed can't be used, or null.
export function portError(text) {
  const value = String(text ?? "").trim();
  if (!/^\d+$/.test(value)) return "The port is a number from 1024 to 65535.";
  const port = Number(value);
  if (port < 1024 || port > 65535) return "Choose a port from 1024 to 65535.";
  return null;
}

function isIpv4(text) {
  const parts = text.split(".");
  return parts.length === 4 && parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255);
}

function isIpv6(text) {
  return text.includes(":") && /^[0-9a-fA-F:.]+$/.test(text) && (text.match(/::/g) ?? []).length <= 1;
}

// Why a LAN address the user typed can't be used, or null.
export function addressError(text, reach) {
  if (reach !== "lan") return null;
  const value = String(text ?? "").trim();
  if (!value) return "Enter this computer's address on the network.";
  if (!isIpv4(value) && !isIpv6(value)) return "Enter an IP address, such as 192.168.1.20.";
  if (value === "0.0.0.0" || /^[0:]+$/.test(value)) {
    return "Enter one address of this computer, not every address.";
  }
  if (!isLocalNetwork(value)) {
    return "That address is reachable from the internet. Use a local-network one, such as 192.168.x.x or a Tailscale 100.x address.";
  }
  return null;
}

// The same ranges the backend accepts: RFC 1918, 100.64.0.0/10, link-local,
// unique local (fc00::/7) and loopback.
function isLocalNetwork(value) {
  if (isIpv4(value)) {
    const [a, b] = value.split(".").map(Number);
    return a === 10 || a === 127 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)
      || (a === 169 && b === 254) || (a === 100 && b >= 64 && b <= 127);
  }
  const first = parseInt(value.split(":")[0] || "0", 16);
  return value === "::1" || (first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80;
}

// What `remote_configure` takes, from the form's fields.
export function settingsInput(form) {
  return {
    enabled: Boolean(form.enabled),
    reach: form.reach,
    address: form.reach === "lan" ? String(form.address ?? "").trim() : "",
    port: Number(String(form.port ?? "").trim()),
  };
}

// Address, port and reach can change only while remote control is off, so
// an edit never drops a connected device by surprise.
export function canEditConnection(status) {
  return !status?.enabled;
}
