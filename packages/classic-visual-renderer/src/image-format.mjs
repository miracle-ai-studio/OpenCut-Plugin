import { integer, reject } from "./validation.mjs";
const invalid = () => reject("INVALID_DECODED_IMAGE", "Static image encoding is malformed");
const unsupported = () => reject("UNSUPPORTED_RENDER_FEATURE", "Animated or multiple-image encodings are unsupported");
export function staticImageHeader(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.length >= 8 && [137,80,78,71,13,10,26,10].every((v,i) => bytes[i] === v)) {
    let at = 8, width, height, end = false, data = false;
    while (at + 12 <= bytes.length) {
      const length = view.getUint32(at), type = String.fromCharCode(...bytes.subarray(at+4, at+8));
      if (length > bytes.length - at - 12) invalid();
      if (at === 8 && (type !== "IHDR" || length !== 13)) invalid();
      if (type === "IHDR") { if (width !== undefined) invalid(); width = view.getUint32(at+8); height = view.getUint32(at+12); }
      if (["acTL", "fcTL", "fdAT"].includes(type)) unsupported();
      if (type === "IDAT") data = true;
      at += length + 12;
      if (type === "IEND") { if (length !== 0 || at !== bytes.length) invalid(); end = true; break; }
    }
    if (!end || !data) invalid();
    return { mime: "image/png", width: integer(width,"PNG width",1), height: integer(height,"PNG height",1) };
  }
  if (bytes.length >= 4 && bytes[0] === 255 && bytes[1] === 216) {
    let at = 2, width, height, foundEnd = false;
    // Parse markers including entropy stuffing/restarts. MPF identifies JPEG
    // multiple-picture containers; no first-image fallback is admitted.
    while (at < bytes.length) {
      if (bytes[at++] !== 255) continue;
      while (at < bytes.length && bytes[at] === 255) at++;
      const marker = bytes[at++];
      if (marker === 0 || marker >= 208 && marker <= 215) continue;
      if (marker === 217) { foundEnd = true; if (at !== bytes.length) unsupported(); break; }
      if (marker === 216) unsupported();
      if (at+2 > bytes.length) invalid();
      const length = view.getUint16(at);
      if (length < 2 || length > bytes.length-at) invalid();
      if (marker === 226 && length >= 6 && String.fromCharCode(...bytes.subarray(at+2,at+6)) === "MPF\0") unsupported();
      if ([192,193,194,195,197,198,199,201,202,203,205,206,207].includes(marker)) {
        if (length < 8 || width !== undefined) invalid();
        height = view.getUint16(at+3); width = view.getUint16(at+5);
      }
      at += length;
    }
    if (!foundEnd) invalid();
    return { mime:"image/jpeg", width:integer(width,"JPEG width",1),height:integer(height,"JPEG height",1) };
  }
  unsupported();
}
export function decoderSurfaces(admission) {
  const count=admission.decoderSurfaces??4;
  if(count!==1&&count!==2&&count!==4)reject("INVALID_VISUAL_PLAN","Unknown decoded surface admission");
  return count;
}
export function admitDimensions(width,height,admission) {
  integer(width,"decoded width",1);integer(height,"decoded height",1);
  if (width > admission.maxWidth || height > admission.maxHeight || BigInt(width)*BigInt(height)>BigInt(admission.maxPixels) || BigInt(width)*BigInt(height)*BigInt(decoderSurfaces(admission)*4)>BigInt(admission.maxDecodedBytes)) reject("VISUAL_BUDGET_EXCEEDED","Actual decoded dimensions exceed admission");
}
