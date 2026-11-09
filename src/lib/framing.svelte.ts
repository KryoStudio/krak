// The vertical (9:16) framing of the clip open in the editor. The editor loads and saves it with
// the trim (`edits.ts`), the frame over the player moves it, the export dialog uses it.

export const framing = $state({
  /** Export a vertical 9:16 window of the picture. */
  vertical: false,
  /** Where the window is across the picture: 0 against the left edge, 1 the right, 0.5 centred
   *  (what the backend's `cropX` means). */
  x: 0.5,
});

/** Whether a picture of `width` × `height` is wider than 9:16, so a vertical window can be cut
 *  out of it (the backend keeps narrower pictures whole). */
export function canFrame(width: number, height: number): boolean {
  return width > 0 && height > 0 && (height * 9) / 16 < width * 0.99;
}

/** The window's width as a share of the picture's width. */
export function windowShare(width: number, height: number): number {
  return Math.min(1, (height * 9) / 16 / width);
}
