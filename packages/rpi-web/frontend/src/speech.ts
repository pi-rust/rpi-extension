// Turning a Markdown reply into speech, and speech into bounded requests.
//
// The read-aloud path used to send the raw Markdown of the whole message to the
// synthesizer: code fences, table pipes and URLs were spoken aloud, and nothing
// played until the entire reply had been synthesized. These helpers keep only
// the prose and cut it into sentence-sized requests so playback can start after
// the first chunk.

/** Upper bound for one synthesis request: a few sentences, not a reply. */
export const MAX_SPEECH_CHARS = 320;

/**
 * Strip Markdown so only speakable prose remains.
 *
 * Code (fenced or inline), images, bare URLs, HTML, table pipes and list/heading
 * markers are source text, not something a reader should hear.
 */
export function speakableText(markdown: string): string {
  let text = markdown;
  // Fenced code first: a fence may contain anything, including link syntax.
  text = text.replace(
    /^[ \t]*(`{3,}|~{3,})[^\n]*\n[\s\S]*?^[ \t]*\1[^\n]*$/gm,
    "\n",
  );
  // An unterminated fence runs to the end of the message.
  text = text.replace(/^[ \t]*(`{3,}|~{3,})[^\n]*\n[\s\S]*$/m, "\n");
  text = text.replace(/<!--[\s\S]*?-->/g, "");
  text = text.replace(/!\[[^\]]*\]\([^)]*\)/g, "");
  // Double-backtick spans may contain single backticks.
  text = text.replace(/``[^`\n]+``/g, "");
  text = text.replace(/`[^`\n]*`/g, "");
  // Autolinks and bare URLs are read as letters, not as an address.
  text = text.replace(/<(?:https?|mailto):[^>\s]+>/gi, "");
  text = text.replace(/https?:\/\/[^\s)>\]]+/gi, "");
  text = text.replace(/www\.[^\s)>\]]+/gi, "");
  // Links keep their label; footnote markers and definitions disappear.
  text = text.replace(/\[([^\]]+)\]\([^)]*\)/g, "$1");
  text = text.replace(/\[([^\]]+)\]\[[^\]]*\]/g, "$1");
  text = text.replace(/\[\^[^\]]*\]/g, "");
  text = text.replace(/^[ \t]*\[[^\]]+\]:[^\n]*$/gm, "");
  // Table separator rows go; body rows become comma-separated cells.
  text = text.replace(
    /^[ \t]*\|?[ \t]*:?-{3,}:?[ \t]*(\|[ \t]*:?-{3,}:?[ \t]*)*\|?[ \t]*$/gm,
    "",
  );
  text = text.replace(/^[ \t]*\|(.+)\|[ \t]*$/gm, (_, row: string) =>
    row
      .split("|")
      .map((cell) => cell.trim())
      .filter(Boolean)
      .join("，"),
  );
  // Block structure: headings, quotes, task boxes and list markers.
  text = text.replace(/^[ \t]{0,3}#{1,6}[ \t]+/gm, "");
  text = text.replace(/^[ \t]{0,3}>[ \t]?/gm, "");
  text = text.replace(/^[ \t]*[-*+][ \t]+\[[ xX]\][ \t]+/gm, "");
  text = text.replace(/^[ \t]*[-*+][ \t]+/gm, "");
  text = text.replace(/^[ \t]*\d+[.)][ \t]+/gm, "");
  // Emphasis markers; a paired marker only, so `snake_case` survives.
  text = text.replace(/(\*\*|__)(?=\S)([\s\S]*?\S)\1/g, "$2");
  text = text.replace(/(\*|_)(?=\S)([^*_\n]*?\S)\1/g, "$2");
  text = text.replace(/~~(?=\S)([\s\S]*?\S)~~/g, "$1");
  text = text.replace(/==(?=\S)([\s\S]*?\S)==/g, "$1");
  // HTML and the entities Markdown escapes.
  text = text.replace(/<[^>]+>/g, "");
  text = text
    .replace(/&nbsp;/gi, " ")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'")
    .replace(/&amp;/gi, "&");
  // Keep paragraph breaks (one blank line) so the chunker can pause there.
  const lines: string[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.replace(/[ \t]+/g, " ").trim();
    if (line) lines.push(line);
    else if (lines.length > 0 && lines[lines.length - 1] !== "") lines.push("");
  }
  return lines.join("\n").trim();
}

/** Sentence-sized request bodies, in reading order. */
export function splitForSpeech(
  text: string,
  maxChars = MAX_SPEECH_CHARS,
): string[] {
  const chunks: string[] = [];
  let current = "";
  const flush = () => {
    const value = current.trim();
    if (value) chunks.push(value);
    current = "";
  };
  for (const paragraph of text.split(/\n{2,}/)) {
    const paragraphText = paragraph.replace(/\s*\n\s*/g, " ").trim();
    if (!paragraphText) continue;
    for (const sentence of splitSentences(paragraphText)) {
      for (const piece of breakLongSentence(sentence, maxChars)) {
        if (!current) current = piece;
        else {
          const separator = joinSeparator(current, piece);
          if (current.length + separator.length + piece.length <= maxChars)
            current += `${separator}${piece}`;
          else {
            flush();
            current = piece;
          }
        }
      }
    }
    // Paragraphs never share a chunk: they are the natural pause.
    flush();
  }
  flush();
  return chunks;
}

/** Latin text needs a space between sentences; CJK punctuation already pauses. */
function joinSeparator(left: string, right: string) {
  return /[A-Za-z0-9)\]"'.:!?;]$/.test(left) && /^[A-Za-z0-9("'\[]/.test(right)
    ? " "
    : "";
}

function splitSentences(text: string): string[] {
  return text
    .split(/(?<=[。！？!?；;])|(?<=\.)\s+/)
    .map((sentence) => sentence.trim())
    .filter(Boolean);
}

/** Prefer clause/word boundaries; only hard-cut text that has neither. */
function breakLongSentence(sentence: string, maxChars: number): string[] {
  const parts: string[] = [];
  let rest = sentence;
  while (rest.length > maxChars) {
    const window = rest.slice(0, maxChars);
    let cut = -1;
    for (const boundary of [/(?<=[，,、])/g, /(?<=\s)/g]) {
      const matches = [...window.matchAll(boundary)];
      const last = matches[matches.length - 1];
      // Lookbehind boundaries are zero-width: the match index is already the
      // position just after the clause mark.
      if (last && last.index > 0) {
        cut = last.index;
        break;
      }
    }
    if (cut <= 0) cut = [...window].length;
    const piece = rest.slice(0, cut).trim();
    if (piece) parts.push(piece);
    rest = rest.slice(cut).trim();
  }
  if (rest) parts.push(rest);
  return parts;
}
