import type { ImageState } from "./image";

export type StatusInput = {
  image: ImageState | null;
  /** Opening, cancelling or a failure reported outside the image state. */
  message: { text: string; error: boolean } | null;
  saving: boolean;
  saveError: string | null;
  saveNotice: string | null;
  savedPath: string | null;
  dirty: boolean;
  annotationError: string | null;
  tileError: string | null;
};

const failure = (text: string) => ({ text, title: text, error: true });
const notice = (text: string, title = text) => ({ text, title, error: false });

/** Choose the one status bar message for the current image, most urgent first. */
export function statusFor(status: StatusInput) {
  const { image } = status;
  const detail = (text: string) => notice(text, image?.detail || text);
  if (status.saveError) return failure(status.saveError);
  if (status.message)
    return status.message.error
      ? failure(status.message.text)
      : notice(status.message.text);
  if (status.saving) return detail("Saving annotations…");
  if (image?.phase === "error")
    return failure(image.error || "Could not load image");
  if (status.annotationError) return failure(status.annotationError);
  if (image?.phase === "cancelled") return detail("Cancelled");
  if (status.tileError) return failure(status.tileError);
  if (status.saveNotice) return notice(status.saveNotice);
  if (image?.phase === "loading")
    return detail(
      image.previewReady
        ? "Loading detail…"
        : image.width > 0
          ? "Building preview…"
          : "Reading image…",
    );
  if (status.savedPath && !status.dirty) return notice("Saved");
  if (image?.readOnly) return detail("Browsing only · annotations unavailable");
  return detail("");
}
