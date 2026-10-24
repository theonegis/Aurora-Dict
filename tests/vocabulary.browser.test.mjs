import assert from "node:assert/strict";
import { test } from "node:test";

// Run with Vite on 1421 and Chrome remote debugging on 9228.
const chrome = "http://127.0.0.1:9228";
const fixture = "http://127.0.0.1:1421/tests/vocabulary.browser.html";

async function openFixture({ empty = false, modelMissing = false, fail = false, width = null } = {}) {
  const search = new URLSearchParams();
  if (empty) search.set("empty", "1");
  if (modelMissing) search.set("modelMissing", "1");
  if (fail) search.set("fail", "1");
  const target = await (await fetch(`${chrome}/json/new?${encodeURIComponent(`${fixture}${search.size ? `?${search}` : ""}`)}`, { method: "PUT" })).json();
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.addEventListener("open", resolve, { once: true }); socket.addEventListener("error", reject, { once: true }); });
  let nextId = 0;
  const pending = new Map();
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (!message.id) return;
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);
  });
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
    return result.result.value;
  };
  if (width) await send("Emulation.setDeviceMetricsOverride", { width, height: 700, deviceScaleFactor: 1, mobile: false });
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await evaluate(`!!document.querySelector('${empty ? ".vocabulary-list-panel" : ".vocabulary-list > button"}')`)) break;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.equal(await evaluate(`!!document.querySelector('${empty ? ".vocabulary-list-panel" : ".vocabulary-list > button"}')`), true, "fixture loaded");
  return { evaluate, close: async () => { socket.close(); await fetch(`${chrome}/json/close/${target.id}`); } };
}

test("delete confirmation in reading card is visibly filled", async () => {
  const page = await openFixture();
  try {
    const style = await page.evaluate(`(() => {
      document.querySelector('.vocabulary-read-actions .danger-button').click();
      return new Promise((resolve) => requestAnimationFrame(() => {
        const button = document.querySelector('.vocabulary-read-actions .danger-button.is-confirming');
        const computed = button && getComputedStyle(button);
        resolve(computed && { text: button.textContent, background: computed.backgroundColor, color: computed.color });
      }));
    })()`);
    assert.equal(style.text, "确认删除");
    assert.notEqual(style.background, "rgba(0, 0, 0, 0)");
    assert.notEqual(style.background, "transparent");
  } finally { await page.close(); }
});

test("right-clicking a preview entry offers edit and delete", async () => {
  const page = await openFixture();
  try {
    const labels = await page.evaluate(`(() => {
      const row = document.querySelector('.vocabulary-list > button');
      row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2, clientX: 80, clientY: 190 }));
      return new Promise((resolve) => requestAnimationFrame(() => resolve([...document.querySelectorAll('.vocabulary-entry-menu [role="menuitem"]')].map((item) => item.textContent.trim()))));
    })()`);
    assert.deepEqual(labels, ["编辑", "删除"]);
  } finally { await page.close(); }
});

test("double-clicking a preview entry opens its editor", async () => {
  const page = await openFixture();
  try {
    const editorWord = await page.evaluate(`(() => {
      const row = document.querySelector('.vocabulary-list > button');
      row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, button: 0 }));
      return new Promise((resolve) => requestAnimationFrame(() => resolve(document.querySelector('.vocabulary-word-fields input')?.value ?? null)));
    })()`);
    assert.equal(editorWord, "serendipity");
  } finally { await page.close(); }
});

test("Delete on a focused preview entry arms confirmation, not immediate removal", async () => {
  const page = await openFixture();
  try {
    const state = await page.evaluate(`(() => {
      const row = document.querySelector('.vocabulary-list > button');
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, key: 'Backspace' }));
      return new Promise((resolve) => requestAnimationFrame(() => resolve({
        confirm: !!document.querySelector('.vocabulary-read-actions .danger-button.is-confirming'),
        removed: window.__vocabularyTest.removed,
      })));
    })()`);
    assert.equal(state.confirm, true);
    assert.deepEqual(state.removed, []);
  } finally { await page.close(); }
});

test("context-menu edit opens the clicked entry, even when another entry is selected", async () => {
  const page = await openFixture();
  try {
    const word = await page.evaluate(`(() => {
      document.querySelectorAll('.vocabulary-list > button')[1].dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2, clientX: 80, clientY: 240 }));
      return new Promise((resolve) => requestAnimationFrame(() => {
        document.querySelector('.vocabulary-entry-menu [role="menuitem"]').click();
        requestAnimationFrame(() => resolve(document.querySelector('.vocabulary-word-fields input')?.value ?? null));
      }));
    })()`);
    assert.equal(word, "kindness");
  } finally { await page.close(); }
});

test("context-menu delete confirms and removes only the clicked entry", async () => {
  const page = await openFixture();
  try {
    const state = await page.evaluate(`(() => {
      document.querySelectorAll('.vocabulary-list > button')[1].dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2, clientX: 80, clientY: 240 }));
      return new Promise((resolve) => requestAnimationFrame(() => {
        document.querySelector('.vocabulary-entry-menu .is-danger').click();
        requestAnimationFrame(() => {
          const wordBeforeConfirm = document.querySelector('.vocabulary-reading > .word-line > h3')?.textContent;
          const removedBeforeConfirm = [...window.__vocabularyTest.removed];
          document.querySelector('.vocabulary-read-actions .is-confirming').click();
          setTimeout(() => resolve({ wordBeforeConfirm, removedBeforeConfirm, removed: window.__vocabularyTest.removed, remaining: [...document.querySelectorAll('.vocabulary-list > button b')].map((item) => item.textContent) }), 30);
        });
      }));
    })()`);
    assert.equal(state.wordBeforeConfirm, "kindness");
    assert.deepEqual(state.removedBeforeConfirm, []);
    assert.deepEqual(state.removed, [2]);
    assert.deepEqual(state.remaining, ["serendipity"]);
  } finally { await page.close(); }
});

test("Backspace in the vocabulary search field never arms deletion", async () => {
  const page = await openFixture();
  try {
    const state = await page.evaluate(`(() => {
      const input = document.querySelector('.vocabulary-list-actions input');
      input.focus();
      input.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, key: 'Backspace' }));
      return new Promise((resolve) => requestAnimationFrame(() => resolve({ confirm: !!document.querySelector('.vocabulary-read-actions .is-confirming'), removed: window.__vocabularyTest.removed })));
    })()`);
    assert.equal(state.confirm, false);
    assert.deepEqual(state.removed, []);
  } finally { await page.close(); }
});

test("preview cards have equal left and right insets inside the summary panel", async () => {
  const page = await openFixture();
  try {
    const insets = await page.evaluate(`(() => {
      const panel = document.querySelector('.vocabulary-list-panel').getBoundingClientRect();
      const row = document.querySelector('.vocabulary-list > button').getBoundingClientRect();
      return { left: row.left - panel.left, right: panel.right - row.right };
    })()`);
    assert.ok(Math.abs(insets.left - insets.right) <= 1, JSON.stringify(insets));
  } finally { await page.close(); }
});

test("both vocabulary cards have in-card headings in read and edit mode", async () => {
  const page = await openFixture();
  try {
    const headings = await page.evaluate(`(() => ({
      left: document.querySelector('.vocabulary-list-panel > header')?.textContent.trim() ?? null,
      right: document.querySelector('.vocabulary-reading > header')?.textContent.trim() ?? null,
    }))()`);
    assert.deepEqual(headings, { left: "生词汇总", right: "单词详解" });
    const editHeading = await page.evaluate(`(() => {
      document.querySelector('.vocabulary-list > button').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
      return new Promise((resolve) => requestAnimationFrame(() => resolve(document.querySelector('.vocabulary-editor-card > header')?.textContent.trim() ?? null)));
    })()`);
    assert.equal(editHeading, "单词详解");
  } finally { await page.close(); }
});

test("empty vocabulary still keeps its summary heading inside a card", async () => {
  const page = await openFixture({ empty: true });
  try {
    const card = await page.evaluate(`(() => {
      const panel = document.querySelector('.vocabulary-list-panel');
      const style = getComputedStyle(panel);
      return { heading: panel.querySelector(':scope > header')?.textContent.trim(), borderWidth: style.borderTopWidth, background: style.backgroundColor };
    })()`);
    assert.equal(card.heading, "生词汇总");
    assert.notEqual(card.borderWidth, "0px");
    assert.notEqual(card.background, "rgba(0, 0, 0, 0)");
  } finally { await page.close(); }
});

test("card titles precede the word in the heading hierarchy", async () => {
  const page = await openFixture();
  try {
    const headings = await page.evaluate(`[...document.querySelectorAll('.vocabulary-list-panel > header > *, .vocabulary-reading > header > *, .vocabulary-reading > .word-line > h3')].map((heading) => [heading.tagName, heading.textContent.trim()])`);
    assert.deepEqual(headings, [["H2", "生词汇总"], ["H2", "单词详解"], ["H3", "serendipity"]]);
  } finally { await page.close(); }
});

test("quiz shows immediate feedback, preserves a question across view switches, and summarizes wrong words", async () => {
  const page = await openFixture();
  try {
    const outcome = await page.evaluate(`(async () => {
      const tab = (name) => [...document.querySelectorAll('.quick-actions button')].find((button) => button.textContent === name);
      tab('背单词').click();
      const count = document.querySelector('.vocabulary-quiz-count input');
      count.value = '2'; count.dispatchEvent(new Event('input', { bubbles: true }));
      document.querySelector('.vocabulary-quiz-card > .primary-button').click();
      for (let i = 0; i < 100 && !document.querySelector('.vocabulary-quiz-options button'); i++) await new Promise((resolve) => setTimeout(resolve, 10));
      const before = document.querySelector('.vocabulary-quiz-sentence')?.textContent;
      tab('生词本').click(); tab('背单词').click();
      const after = document.querySelector('.vocabulary-quiz-sentence')?.textContent;
      document.querySelectorAll('.vocabulary-quiz-options button')[0].click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
      const feedback = document.querySelector('.vocabulary-quiz-feedback')?.textContent;
      document.querySelector('.vocabulary-quiz-card > .primary-button').click();
      for (let i = 0; i < 100 && !document.querySelector('.vocabulary-quiz-options button:not(:disabled)'); i++) await new Promise((resolve) => setTimeout(resolve, 10));
      document.querySelectorAll('.vocabulary-quiz-options button')[1].click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
      document.querySelector('.vocabulary-quiz-card > .primary-button').click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
      return { before, after, feedback, result: document.querySelector('.vocabulary-quiz-heading')?.textContent, wrongCards: document.querySelectorAll('.vocabulary-quiz-wrong article').length, generated: window.__vocabularyTest.generated.length };
    })()`);
    assert.equal(outcome.before, "She showed ____ today.");
    assert.equal(outcome.after, outcome.before);
    assert.match(outcome.feedback, /回答错误.*正确答案/);
    assert.match(outcome.result, /练习结果/);
    assert.equal(outcome.wrongCards, 1);
    assert.equal(outcome.generated, 2);
  } finally { await page.close(); }
});

test("generation failure keeps the same word pending until retry", async () => {
  const page = await openFixture({ fail: true });
  try {
    const result = await page.evaluate(`(async () => {
      [...document.querySelectorAll('.quick-actions button')].find((button) => button.textContent === '背单词').click();
      document.querySelector('.vocabulary-quiz-card > .primary-button').click();
      for (let i = 0; i < 100 && !document.querySelector('.vocabulary-quiz-error'); i++) await new Promise((resolve) => setTimeout(resolve, 10));
      const error = document.querySelector('.vocabulary-quiz-error')?.textContent;
      document.querySelector('.vocabulary-quiz-error button').click();
      for (let i = 0; i < 100 && !document.querySelector('.vocabulary-quiz-options'); i++) await new Promise((resolve) => setTimeout(resolve, 10));
      return { error, calls: window.__vocabularyTest.generated, options: document.querySelectorAll('.vocabulary-quiz-options button').length };
    })()`);
    assert.match(result.error, /生成失败/);
    assert.equal(result.calls.length, 2);
    assert.equal(result.calls[0], result.calls[1]);
    assert.equal(result.options, 4);
  } finally { await page.close(); }
});

test("quiz start is disabled without a local model and settings remains reachable", async () => {
  const page = await openFixture({ modelMissing: true });
  try {
    const state = await page.evaluate(`(async () => {
      [...document.querySelectorAll('.quick-actions button')].find((button) => button.textContent === '背单词').click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
      const disabled = document.querySelector('.vocabulary-quiz-card > .primary-button').disabled;
      document.querySelector('.vocabulary-quiz-notice button').click();
      return { disabled, settingsOpened: window.__vocabularyTest.settingsOpened };
    })()`);
    assert.deepEqual(state, { disabled: true, settingsOpened: true });
  } finally { await page.close(); }
});

test("quiz choices fit the default 650px app width", async () => {
  const page = await openFixture({ width: 650 });
  try {
    const bounds = await page.evaluate(`(async () => {
      [...document.querySelectorAll('.quick-actions button')].find((button) => button.textContent === '背单词').click();
      document.querySelector('.vocabulary-quiz-card > .primary-button').click();
      for (let i = 0; i < 100 && !document.querySelector('.vocabulary-quiz-options button'); i++) await new Promise((resolve) => setTimeout(resolve, 10));
      const card = document.querySelector('.vocabulary-quiz-card').getBoundingClientRect();
      const options = [...document.querySelectorAll('.vocabulary-quiz-options button')].map((button) => button.getBoundingClientRect());
      return { viewport: window.innerWidth, cardLeft: card.left, cardRight: card.right, optionLeft: Math.min(...options.map((box) => box.left)), optionRight: Math.max(...options.map((box) => box.right)) };
    })()`);
    assert.equal(bounds.viewport, 650);
    assert.ok(bounds.cardLeft >= 0 && bounds.cardRight <= 650, JSON.stringify(bounds));
    assert.ok(bounds.optionLeft >= bounds.cardLeft && bounds.optionRight <= bounds.cardRight, JSON.stringify(bounds));
  } finally { await page.close(); }
});

test("learner can choose a single-question session", async () => {
  const page = await openFixture();
  try {
    const result = await page.evaluate(`(async () => {
      [...document.querySelectorAll('.quick-actions button')].find((button) => button.textContent === '背单词').click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
      const input = document.querySelector('.vocabulary-quiz-count input');
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, '1');
      input.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => requestAnimationFrame(resolve));
      const available = document.querySelector('.vocabulary-quiz-card > .vocabulary-quiz-note')?.textContent;
      document.querySelector('.vocabulary-quiz-card > .primary-button').click();
      for (let i = 0; i < 100 && !document.querySelector('.vocabulary-quiz-options button'); i++) await new Promise((resolve) => setTimeout(resolve, 10));
      document.querySelectorAll('.vocabulary-quiz-options button')[1].click();
      await new Promise((resolve) => requestAnimationFrame(resolve));
      const finalAction = document.querySelector('.vocabulary-quiz-card > .primary-button')?.textContent;
      return { available, finalAction };
    })()`);
    assert.match(result.available, /^1 \/ 2/);
    assert.equal(result.finalAction, "查看结果");
  } finally { await page.close(); }
});
