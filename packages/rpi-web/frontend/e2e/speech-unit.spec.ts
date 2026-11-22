import { expect, test } from "@playwright/test";
import { MAX_SPEECH_CHARS, speakableText, splitForSpeech } from "../src/speech";

// Pure helpers: no page, no API, no synthesis request.

test("code and markup never become speech", () => {
  const spoken = speakableText(
    [
      "# 标题",
      "",
      "正文里有 `inline_code()` 和 [链接文字](https://example.com/page)。",
      "",
      "```ts",
      "const answer = 42;",
      "console.log(answer);",
      "```",
      "",
      "```mermaid",
      "graph TD; A-->B;",
      "```",
      "",
      "> 引用一句",
      "",
      "- 第一项",
      "- [x] 已完成",
      "1. 有序项",
      "",
      "| 名称 | 数值 |",
      "| --- | --- |",
      "| 苹果 | 3 |",
      "",
      "结尾 http://example.com/x 和 <https://auto.example>。",
    ].join("\n"),
  );
  expect(spoken).not.toContain("const answer");
  expect(spoken).not.toContain("console.log");
  expect(spoken).not.toContain("graph TD");
  expect(spoken).not.toContain("```");
  expect(spoken).not.toContain("`");
  expect(spoken).not.toContain("example.com");
  expect(spoken).toContain("标题");
  expect(spoken).toContain("正文里有");
  expect(spoken).toContain("链接文字");
  expect(spoken).not.toContain("](");
  expect(spoken).toContain("引用一句");
  expect(spoken).toContain("第一项");
  expect(spoken).toContain("已完成");
  expect(spoken).toContain("有序项");
  expect(spoken).toContain("苹果，3");
  expect(spoken).not.toContain("|");
});

test("emphasis, html and entities are read as words", () => {
  const spoken = speakableText(
    "<p>**粗体** 和 *斜体* 与 ~~删除~~ &amp; snake_case</p>",
  );
  expect(spoken).toBe("粗体 和 斜体 与 删除 & snake_case");
});

test("an unterminated fence is dropped to the end", () => {
  const spoken = speakableText("说明\n\n```bash\nrm -rf /\n");
  expect(spoken).toBe("说明");
});

test("a reply with only code has nothing to say", () => {
  expect(speakableText("```ts\nconst a = 1;\n```")).toBe("");
  expect(splitForSpeech("")).toEqual([]);
});

test("speech is split into sentence-sized requests in order", () => {
  const paragraphs = [
    "第一段。第二句话！第三句话？",
    "Second paragraph. It has two sentences.",
    "没有标点的一长段文字".repeat(60),
  ];
  const chunks = splitForSpeech(paragraphs.join("\n\n"));
  expect(chunks.length).toBeGreaterThan(3);
  for (const chunk of chunks) {
    expect(chunk.length).toBeGreaterThan(0);
    expect(chunk.length).toBeLessThanOrEqual(MAX_SPEECH_CHARS);
  }
  // Paragraphs never share a request.
  expect(chunks[0]).toBe("第一段。第二句话！第三句话？");
  expect(chunks[1]).toBe("Second paragraph. It has two sentences.");
  // Concatenated chunks preserve the original words.
  expect(chunks.join("").replace(/\s/g, "")).toContain(
    "没有标点的一长段文字".repeat(5),
  );
});

test("a sentence longer than the limit is cut on a clause boundary", () => {
  const sentence = `${"前半段".repeat(80)}，${"后半段".repeat(100)}。`;
  const chunks = splitForSpeech(sentence);
  expect(chunks.length).toBeGreaterThan(1);
  expect(chunks[0].endsWith("，")).toBe(true);
  for (const chunk of chunks)
    expect(chunk.length).toBeLessThanOrEqual(MAX_SPEECH_CHARS);
});

test("plain prose under the limit stays a single request", () => {
  expect(splitForSpeech("一句话就够。")).toEqual(["一句话就够。"]);
});
