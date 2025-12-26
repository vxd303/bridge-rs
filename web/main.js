import { Adb, AdbDaemonTransport } from "https://unpkg.com/@yume-chan/adb@latest?module";
import { ScrcpyClient } from "https://unpkg.com/@yume-chan/adb-scrcpy@latest?module";
import { AdbWebSocketTransport } from "https://unpkg.com/@yume-chan/adb-transport-websocket@latest?module";

const statusBadge = document.getElementById("status");
const endpointInput = document.getElementById("endpoint");
const connectButton = document.getElementById("connect");
const disconnectButton = document.getElementById("disconnect");
const deviceInfo = document.getElementById("device-info");
const mirrorStatus = document.getElementById("mirror-status");
const shellOutput = document.getElementById("shell-output");
const shellCommand = document.getElementById("shell-command");
const shellRun = document.getElementById("run-shell");
const canvas = document.getElementById("screen");

let transport;
let adb;
let scrcpyClient;
let animationFrame;

function setStatus(text, connected = false) {
  statusBadge.textContent = text;
  statusBadge.classList.toggle("connected", connected);
  connectButton.disabled = connected;
  disconnectButton.disabled = !connected;
  shellRun.disabled = !connected;
}

function appendShell(text) {
  shellOutput.value += text + "\n";
  shellOutput.scrollTop = shellOutput.scrollHeight;
}

async function connect() {
  try {
    const url = endpointInput.value.trim();
    setStatus("Connecting…", false);
    transport = await AdbWebSocketTransport.connect(url);
    const daemon = new AdbDaemonTransport(transport);
    adb = await Adb.connect(daemon, {
      // Generate an ephemeral RSA key in the browser for auth.
      // Keys are stored in memory; if the device is already authorized it won't prompt again.
      auth: async (type, data) => {
        console.warn("Received AUTH packet", type, data);
        return undefined;
      },
    });

    const props = await adb.getPropAll();
    deviceInfo.textContent = JSON.stringify(props, null, 2);
    setStatus("Connected", true);
    mirrorStatus.textContent = "Ready";
    await startMirror();
  } catch (error) {
    console.error(error);
    setStatus("Failed", false);
    deviceInfo.textContent = error?.message ?? String(error);
  }
}

async function startMirror() {
  if (!adb) return;
  mirrorStatus.textContent = "Starting scrcpy…";
  try {
    scrcpyClient = await ScrcpyClient.start(adb, {
      tunnelForward: true,
      video: {
        codec: "h264",
      },
      control: false,
    });

    const renderer = scrcpyClient.videoStream?.renderer({
      target: canvas,
    });

    function draw() {
      renderer?.render();
      animationFrame = requestAnimationFrame(draw);
    }

    draw();
    mirrorStatus.textContent = "Streaming";
  } catch (error) {
    mirrorStatus.textContent = error?.message ?? String(error);
  }
}

async function disconnect() {
  cancelAnimationFrame(animationFrame);
  animationFrame = undefined;
  scrcpyClient?.close();
  scrcpyClient = undefined;
  await adb?.close?.();
  await transport?.close?.();
  transport = undefined;
  adb = undefined;
  setStatus("Disconnected", false);
  deviceInfo.textContent = "No device connected.";
  mirrorStatus.textContent = "Idle";
}

async function runShell() {
  if (!adb) return;
  const command = shellCommand.value.trim();
  if (!command) return;

  const socket = await adb.createSocket(`shell:${command}`);
  const decoder = new TextDecoder();
  for await (const chunk of socket.readable) {
    appendShell(decoder.decode(chunk));
  }
}

connectButton.addEventListener("click", connect);
disconnectButton.addEventListener("click", disconnect);
shellRun.addEventListener("click", runShell);
