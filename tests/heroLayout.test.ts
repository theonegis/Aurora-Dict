import assert from "node:assert/strict";
import { test } from "node:test";
import { build } from "esbuild";
import { renderToStaticMarkup } from "react-dom/server";

test("return to the previous lookup sits beside the dictionary heading", async () => {
  const entry = new URL("../src/App.tsx", import.meta.url);
  const bundle = await build({
    entryPoints: [entry.pathname], bundle: true, platform: "node", format: "esm", write: false,
    define: { "import.meta.url": JSON.stringify(entry.href) },
    loader: { ".css": "empty", ".png": "dataurl", ".jpg": "dataurl", ".jpeg": "dataurl" },
  });
  const { Hero } = await import(`data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString("base64")}`);
  const props = {
    panel: "dictionary",
    setPanel: () => {},
    previousWord: "still",
    returnToPrevious: () => {},
    t: (key: string) => key === "quickDictionaryTip" ? "单词查询" : key === "wordGraphBack" ? "返回 still" : key,
  };
  const markup = renderToStaticMarkup(Hero(props));
  assert.match(markup, /<div class="hero-heading"><h1>单词查询<\/h1><button[^>]*class="lookup-return"[^>]*>.*返回 still<\/span><\/button><\/div>/);
  assert.ok(markup.indexOf('class="lookup-return"') < markup.indexOf('class="quick-actions"'));
  assert.doesNotMatch(renderToStaticMarkup(Hero({ ...props, previousWord: null })), /lookup-return/);
  assert.doesNotMatch(renderToStaticMarkup(Hero({ ...props, panel: "vocabulary" })), /lookup-return/);
});
