// Dropped / pasted / picked files. A file on the Mac EMP runs on is attached where it is
// (`attachments.locateFile`; any size, nothing copied). Pictures are read for
// `attachments.upload` instead, scaled down if big, because the model is shown them inline (model
// APIs take about 5 MB per image). Files that aren't on the Mac (the phone's) are uploaded too.

import type { PreparedFile } from "../store/workspace";
import { bytesToBase64 } from "./base64";

/** Largest upload; matches empd's `attachments::MAX_BYTES`. Files found on the Mac have no limit. */
export const MAX_ATTACHMENT_BYTES = 20 * 1024 * 1024;
const MAX_SIDE = 2560;
const SHRINK_OVER = 3_500_000;

/** Files in a drag or paste, if it carries any (not text or EMP's own thread drags). */
export function transferFiles(dt: DataTransfer | null): File[] {
  return dt ? Array.from(dt.files) : [];
}

/** A dropped folder: web views give its name and contents, never its path (empd finds it). */
export interface DroppedFolder {
  name: string;
  entries: string[];
}

/** Children read per folder: enough to tell same-named folders apart. */
const FOLDER_SAMPLE = 200;

/**
 * Splits a drop into files and folders. Must run during the drop event: the browser forgets
 * the items afterwards. Folders' contents are read after.
 */
export function splitTransfer(dt: DataTransfer | null): { files: File[]; folders: Promise<DroppedFolder[]> } {
  if (!dt) return { files: [], folders: Promise.resolve([]) };
  const items = Array.from(dt.items ?? []).filter((i) => i.kind === "file");
  const entries = items.map((i) => (typeof i.webkitGetAsEntry === "function" ? i.webkitGetAsEntry() : null));
  if (!entries.some((e) => e?.isDirectory)) return { files: transferFiles(dt), folders: Promise.resolve([]) };
  const files: File[] = [];
  const dirs: FileSystemDirectoryEntry[] = [];
  items.forEach((item, i) => {
    const entry = entries[i];
    if (entry?.isDirectory) dirs.push(entry as FileSystemDirectoryEntry);
    else {
      const f = item.getAsFile();
      if (f) files.push(f);
    }
  });
  return { files, folders: Promise.all(dirs.map(readFolder)) };
}

function readFolder(dir: FileSystemDirectoryEntry): Promise<DroppedFolder> {
  return new Promise((resolve) => {
    const done = (list: FileSystemEntry[]) => resolve({ name: dir.name, entries: list.slice(0, FOLDER_SAMPLE).map((e) => e.name) });
    try {
      dir.createReader().readEntries(done, () => done([]));
    } catch {
      done([]);
    }
  });
}

/** The lines that hand folders to the agent; a trailing slash marks a folder. */
export function folderLines(paths: string[]): string {
  return paths.map((p) => `[Attached: ${p.replace(/\/+$/, "")}/]`).join("\n");
}

export function carriesFiles(dt: DataTransfer | null): boolean {
  return !!dt && Array.from(dt.types).includes("Files");
}

const PICTURE = /^image\/(png|jpeg|webp|gif)$/;

export async function prepareFile(file: File): Promise<PreparedFile> {
  const base = { name: file.name || defaultName(file.type), size: file.size, preview: null, data: "", mime: file.type || null };
  const picture = !!base.mime && PICTURE.test(base.mime) && file.size <= MAX_ATTACHMENT_BYTES;
  if (file.size > 0 && file.name && !picture) {
    return { ...base, find: { size: file.size, modified: file.lastModified }, upload: file.size <= MAX_ATTACHMENT_BYTES ? () => readFile(file) : undefined };
  }
  return readFile(file);
}

/** The file's bytes for `attachments.upload` (pictures shrunk). */
async function readFile(file: File): Promise<PreparedFile> {
  const base = { name: file.name || defaultName(file.type), size: file.size, preview: null, data: "", mime: file.type || null };
  if (file.size > MAX_ATTACHMENT_BYTES) {
    return { ...base, error: `${base.name} is ${Math.round(file.size / 1048576)} MB; uploads can be at most 20 MB` };
  }
  let bytes: Uint8Array;
  try {
    bytes = new Uint8Array(await file.arrayBuffer());
  } catch {
    return { ...base, error: `Couldn't read ${base.name}. Folders can't be attached; drop the files inside instead.` };
  }
  if (bytes.length === 0) return { ...base, error: `${base.name} is empty (folders can't be attached)` };
  let { name, mime } = base;
  if (mime && /^image\/(png|jpeg|webp)$/.test(mime)) {
    const shrunk = await shrinkImage(file, bytes.length).catch(() => null);
    if (shrunk) {
      bytes = shrunk.bytes;
      if (shrunk.mime !== mime) name = name.replace(/\.\w+$/, "") + (shrunk.mime === "image/jpeg" ? ".jpg" : ".png");
      mime = shrunk.mime;
    }
  }
  const data = bytesToBase64(bytes);
  const preview = mime?.startsWith("image/") ? `data:${mime};base64,${data}` : null;
  return { name, mime, data, size: bytes.length, preview };
}

async function shrinkImage(file: File, size: number): Promise<{ bytes: Uint8Array; mime: string } | null> {
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, MAX_SIDE / Math.max(bitmap.width, bitmap.height));
  if (scale === 1 && size <= SHRINK_OVER) {
    bitmap.close();
    return null;
  }
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(bitmap.width * scale);
  canvas.height = Math.round(bitmap.height * scale);
  canvas.getContext("2d")?.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  let blob = await toBlob(canvas, "image/png");
  if (blob && blob.size > SHRINK_OVER) blob = await toBlob(canvas, "image/jpeg", 0.88);
  if (!blob) return null;
  return { bytes: new Uint8Array(await blob.arrayBuffer()), mime: blob.type };
}

function toBlob(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> {
  return new Promise((resolve) => canvas.toBlob(resolve, type, quality));
}

/** Pasted screenshots arrive nameless. */
function defaultName(mime: string): string {
  const ext = mime.split("/")[1]?.replace("jpeg", "jpg");
  return mime.startsWith("image/") ? `pasted-image.${ext ?? "png"}` : "file";
}

/** `1.2 MB`, `340 KB`. */
export function formatSize(bytes: number): string {
  if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/** Splits `[Attached: <path>]` lines (empd appends files', the composer folders') off a user message. */
export function splitAttachments(text: string): { text: string; files: string[] } {
  const files: string[] = [];
  const lines = text.split("\n");
  while (lines.length) {
    const last = lines[lines.length - 1]!.trim();
    const m = /^\[Attached: (.+)\]$/.exec(last);
    // Folder and file lines arrive as two blocks with a blank line between.
    if (!m && !(last === "" && files.length > 0)) break;
    if (m) files.unshift(m[1]!);
    lines.pop();
  }
  return { text: lines.join("\n").trimEnd(), files };
}
