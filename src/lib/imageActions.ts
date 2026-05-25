import { save } from "@tauri-apps/plugin-dialog";
import { useBoardStore } from "../store/board";
import { useChatStore } from "../store/chat";
import { ipc } from "./ipc";
import { buildOverlays, buildMarkNotes } from "./overlay";

// Preset prompts — ALWAYS English (agent-facing; UI language independent).
export const PRESET_REMOVE_BG =
  "Remove the background from this image — keep only the subject, and make the background transparent (PNG) or solid white.";
export const PRESET_UPSCALE =
  "Increase this image's sharpness and resolution so it looks crisper and more detailed, keeping the content and composition unchanged.";

/** A placement's filename (export default + display). */
export function imageName(pid: string): string {
  const { placements, assets } = useBoardStore.getState();
  const p = placements.get(pid);
  const a = p && assets.get(p.assetId);
  return a ? (a.path.split("/").pop() ?? "image.png") : "image.png";
}

/** Run a preset prompt against one image (去背景 / 变高清). Any marks on the image
 *  are composed into the message and consumed after the turn is sent. */
export function runImagePreset(boardId: string, pid: string, presetPrompt: string): void {
  const notes = buildMarkNotes([pid]);
  const instruction = [notes, presetPrompt].filter((s) => s.trim()).join("\n\n");
  const turn = useChatStore.getState().startTurn(instruction, [pid]);
  void (async () => {
    try {
      const ov = await buildOverlays(boardId, [pid]);
      await ipc.sendMessage(boardId, instruction, [pid], ov);
      useBoardStore.getState().consumeMarks([pid]);
    } catch (e) {
      const message = `Could not send this preset: ${e instanceof Error ? e.message : String(e)}`;
      useChatStore.getState().failTurn(message, turn);
      void ipc.frontLog("error", message).catch(() => {});
    }
  })();
}

/** Export one image via a native save dialog. */
export async function exportImage(boardId: string, pid: string): Promise<void> {
  const dest = await save({ defaultPath: imageName(pid), title: "Export image" });
  if (typeof dest === "string") await ipc.exportAsset(boardId, pid, dest);
}
