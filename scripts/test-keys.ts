/**
 * Keyboard encoder and app-chord tests. `npm run test:keys`.
 *
 * Plain `node:test` on Node's built-in type stripping, the same way
 * `bench-filter.ts` runs — no test framework dependency for ~100 lines of
 * pure functions. These are pure: they take a KeyboardEvent-shaped object and
 * return bytes, so a plain object stands in for the DOM event.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { keyToBytes } from "../src/lib/keys.ts";
import { isAppChord, keyIs } from "../src/lib/platform.ts";

type Ev = Partial<KeyboardEvent> & { key: string };

/** A KeyboardEvent stand-in. `navigator` is absent under Node, so platform.ts
 *  resolves to the Linux/Windows bindings, which is what is under test. */
function ev(e: Ev): KeyboardEvent {
  return {
    code: "",
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    isComposing: false,
    keyCode: 0,
    getModifierState: () => false,
    ...e,
  } as unknown as KeyboardEvent;
}

const str = (b: Uint8Array | null) => (b === null ? null : new TextDecoder().decode(b));

test("arrows honour DECCKM", () => {
  assert.equal(str(keyToBytes(ev({ key: "ArrowUp" }))), "\x1b[A");
  assert.equal(str(keyToBytes(ev({ key: "ArrowUp" }), { appCursor: true })), "\x1bOA");
  assert.equal(str(keyToBytes(ev({ key: "Home" }), { appCursor: true })), "\x1bOH");
});

test("modified cursor keys use the xterm parameter form, in either mode", () => {
  const ctrlLeft = ev({ key: "ArrowLeft", ctrlKey: true });
  assert.equal(str(keyToBytes(ctrlLeft)), "\x1b[1;5D");
  assert.equal(str(keyToBytes(ctrlLeft, { appCursor: true })), "\x1b[1;5D");
  assert.equal(str(keyToBytes(ev({ key: "ArrowRight", altKey: true }))), "\x1b[1;3C");
  assert.equal(str(keyToBytes(ev({ key: "End", shiftKey: true }))), "\x1b[1;2F");
});

test("function and editing keys", () => {
  assert.equal(str(keyToBytes(ev({ key: "F1" }))), "\x1bOP");
  assert.equal(str(keyToBytes(ev({ key: "F1", ctrlKey: true }))), "\x1b[1;5P");
  assert.equal(str(keyToBytes(ev({ key: "F5" }))), "\x1b[15~");
  assert.equal(str(keyToBytes(ev({ key: "Delete", ctrlKey: true }))), "\x1b[3;5~");
  assert.equal(str(keyToBytes(ev({ key: "Tab", shiftKey: true }))), "\x1b[Z");
  assert.equal(str(keyToBytes(ev({ key: "Backspace" }))), "\x7f");
  assert.equal(str(keyToBytes(ev({ key: "Backspace", ctrlKey: true }))), "\x08");
  assert.equal(str(keyToBytes(ev({ key: "Backspace", altKey: true }))), "\x1b\x7f");
});

test("Ctrl+letters reach the shell, including the ones the app used to steal", () => {
  for (const [k, byte] of [["w", 0x17], ["k", 0x0b], ["b", 0x02], ["i", 0x09], ["c", 0x03]] as const) {
    assert.deepEqual(keyToBytes(ev({ key: k, ctrlKey: true })), new Uint8Array([byte]), `Ctrl+${k}`);
  }
  assert.deepEqual(keyToBytes(ev({ key: "_", ctrlKey: true, shiftKey: true })), new Uint8Array([0x1f]));
  assert.deepEqual(keyToBytes(ev({ key: "b", ctrlKey: true, altKey: true })), new Uint8Array([0x1b, 0x02]));
});

test("Ctrl+C interrupts on a non-Latin layout", () => {
  // Russian layout: the C key produces "с" (Cyrillic es).
  assert.deepEqual(keyToBytes(ev({ key: "с", code: "KeyC", ctrlKey: true })), new Uint8Array([0x03]));
});

test("AltGr characters are typed, not dropped", () => {
  // Windows reports AltGr as Ctrl+Alt; German AltGr+Q is "@".
  const e = ev({ key: "@", ctrlKey: true, altKey: true, getModifierState: (k: string) => k === "AltGraph" });
  assert.equal(str(keyToBytes(e)), "@");
});

test("keys that are not ours are left alone", () => {
  assert.equal(keyToBytes(ev({ key: "Shift", shiftKey: true })), null);
  assert.equal(keyToBytes(ev({ key: "a", metaKey: true })), null);
  assert.equal(keyToBytes(ev({ key: "Process", isComposing: true })), null);
});

test("app chords need Ctrl+Shift off macOS", () => {
  assert.equal(isAppChord(ev({ key: "k", ctrlKey: true }), "k"), false, "plain Ctrl+K is kill-line");
  assert.equal(isAppChord(ev({ key: "K", ctrlKey: true, shiftKey: true }), "k"), true);
  assert.equal(isAppChord(ev({ key: "W", ctrlKey: true, shiftKey: true, altKey: true }), "w"), false);
  assert.equal(isAppChord(ev({ key: "Enter", ctrlKey: true, shiftKey: true }), "Enter"), true);
  // Layout-independent: Ctrl+Shift+C on a Cyrillic layout still copies.
  assert.equal(keyIs(ev({ key: "С", code: "KeyC" }), "c"), true);
  assert.equal(keyIs(ev({ key: "x", code: "KeyC" }), "c"), false, "a Latin key wins over position");
});
