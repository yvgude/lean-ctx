const MIN_LENGTH = 200;
const LEAN_CTX_MARKER = "__lean_ctx_sent__";
let isCompressing = false;
let extensionSettings = { enabled: true, autoCompressPaste: true };

const SITE_CONFIG = {
  "chatgpt.com": {
    input: 'div#prompt-textarea[contenteditable="true"], textarea',
    send: 'button[data-testid="send-button"], button[data-testid="composer-send-button"]',
  },
  "chat.openai.com": {
    input: 'div#prompt-textarea[contenteditable="true"], textarea',
    send: 'button[data-testid="send-button"]',
  },
  "claude.ai": {
    input: 'div.ProseMirror[contenteditable="true"]',
    send: 'button[aria-label="Send Message"], button[aria-label="Send message"]',
  },
  "gemini.google.com": {
    input: 'div.ql-editor[contenteditable="true"], rich-textarea .ql-editor',
    send: 'button[aria-label="Send message"], button.send-button',
  },
  "github.com": {
    input: 'textarea[name="message"], textarea.js-copilot-chat-input',
    send: 'button[type="submit"]',
  },
  "lovable.dev": {
    input: 'textarea, div[contenteditable="true"]',
    send: 'button[type="submit"], button[aria-label*="Send"], button[aria-label*="send"]',
  },
  "bolt.new": {
    input: 'textarea, div[contenteditable="true"]',
    send: 'button[type="submit"], button[aria-label*="Send"]',
  },
  "v0.dev": {
    input: 'textarea, div[contenteditable="true"]',
    send: 'button[type="submit"], button[aria-label*="Send"]',
  },
  "poe.com": {
    input: 'textarea, div[contenteditable="true"]',
    send: 'button[class*="send"], button[aria-label*="Send"]',
  },
  "aistudio.google.com": {
    input: 'textarea, div[contenteditable="true"]',
    send: 'button[aria-label*="Send"], button[aria-label*="Run"]',
  },
  "labs.perplexity.ai": {
    input: 'textarea, div[contenteditable="true"]',
    send: 'button[aria-label*="Submit"], button[aria-label*="Send"]',
  },
};

function getSiteConfig() {
  const host = window.location.hostname;
  for (const [domain, config] of Object.entries(SITE_CONFIG)) {
    if (host.includes(domain)) return config;
  }
  return null;
}

function getInputText(el) {
  if (el.tagName === "TEXTAREA" || el.tagName === "INPUT") return el.value;
  return el.innerText || el.textContent || "";
}

function setTextareaValue(el, text) {
  const proto = Object.getOwnPropertyDescriptor(
    window.HTMLTextAreaElement.prototype,
    "value"
  );
  if (proto && proto.set) {
    proto.set.call(el, text);
  } else {
    el.value = text;
  }
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
}

function setContentEditableText(el, text) {
  el.focus();
  const selection = window.getSelection();
  const range = document.createRange();
  range.selectNodeContents(el);
  selection.removeAllRanges();
  selection.addRange(range);
  document.execCommand("insertText", false, text);
}

function setInputText(el, text) {
  if (el.tagName === "TEXTAREA" || el.tagName === "INPUT") {
    setTextareaValue(el, text);
  } else {
    setContentEditableText(el, text);
  }
}

async function compressText(text) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage({ action: "compress", text }, (response) => {
      if (chrome.runtime.lastError) {
        resolve(null);
        return;
      }
      resolve(response);
    });
  });
}

function findSendButton(config) {
  if (config.send) {
    const btn = document.querySelector(config.send);
    if (btn && !btn.disabled) return btn;
  }
  const fallbacks = [
    'button[type="submit"]',
    'button[aria-label*="Send"]',
    'button[aria-label*="send"]',
  ];
  for (const sel of fallbacks) {
    const btn = document.querySelector(sel);
    if (btn && !btn.disabled) return btn;
  }
  return null;
}

function triggerSend(input, config) {
  const sendBtn = findSendButton(config);
  if (sendBtn) {
    setTimeout(() => sendBtn.click(), 80);
  } else {
    setTimeout(() => {
      const ev = new KeyboardEvent("keydown", {
        key: "Enter",
        code: "Enter",
        keyCode: 13,
        which: 13,
        bubbles: true,
        cancelable: true,
      });
      Object.defineProperty(ev, LEAN_CTX_MARKER, { value: true });
      input.dispatchEvent(ev);
    }, 80);
  }
}

async function handleSubmit(input, config) {
  const text = getInputText(input).trim();
  if (text.length < MIN_LENGTH) {
    triggerSend(input, config);
    return false;
  }

  isCompressing = true;
  showCompressing();

  const response = await compressText(text);

  if (response && !response.skipped && response.compressed && response.compressed !== text) {
    setInputText(input, response.compressed);
    showSavings(response.inputTokens || 0, response.outputTokens || 0, response.savings || 0);
    updateStats(response);
    await new Promise((r) => setTimeout(r, 60));
  }

  hideCompressing();
  isCompressing = false;
  triggerSend(input, config);
  return true;
}

function hookInput(input, config) {
  if (input.dataset.leanCtxHooked) return;
  input.dataset.leanCtxHooked = "true";

  input.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Enter" || e.shiftKey || e.isComposing) return;
      if (!extensionSettings.enabled) return;
      if (isCompressing) {
        e.preventDefault();
        e.stopImmediatePropagation();
        return;
      }
      if (e[LEAN_CTX_MARKER]) return;

      const text = getInputText(input).trim();
      if (text.length < MIN_LENGTH) return;

      e.preventDefault();
      e.stopImmediatePropagation();
      handleSubmit(input, config);
    },
    true
  );

  input.addEventListener("paste", async (e) => {
    if (!extensionSettings.enabled || !extensionSettings.autoCompressPaste) return;

    const text = e.clipboardData?.getData("text/plain");
    if (!text || text.length < MIN_LENGTH) return;

    const response = await compressText(text);
    if (!response || response.skipped || !response.compressed || response.compressed === text)
      return;

    e.preventDefault();

    if (input.tagName === "TEXTAREA" || input.tagName === "INPUT") {
      const start = input.selectionStart || 0;
      const before = input.value.substring(0, start);
      const after = input.value.substring(input.selectionEnd || start);
      setTextareaValue(input, before + response.compressed + after);
    } else {
      document.execCommand("insertText", false, response.compressed);
    }

    showSavings(response.inputTokens || 0, response.outputTokens || 0, response.savings || 0);
    updateStats(response);
  });
}

function hookSendButton(config) {
  const observer = new MutationObserver(() => {
    if (!config.send) return;
    const btns = document.querySelectorAll(config.send);
    btns.forEach((btn) => {
      if (btn.dataset.leanCtxHooked) return;
      btn.dataset.leanCtxHooked = "true";

      btn.addEventListener(
        "click",
        (e) => {
          if (!extensionSettings.enabled) return;
          if (isCompressing || btn.dataset.leanCtxSending) return;

          const input = document.querySelector(config.input);
          if (!input) return;

          const text = getInputText(input).trim();
          if (text.length < MIN_LENGTH) return;

          e.preventDefault();
          e.stopImmediatePropagation();

          btn.dataset.leanCtxSending = "true";
          handleSubmit(input, config).then(() => {
            delete btn.dataset.leanCtxSending;
          });
        },
        true
      );
    });
  });

  observer.observe(document.body, { childList: true, subtree: true });
}

let badge = null;

function createBadge() {
  if (badge) return badge;
  badge = document.createElement("div");
  badge.id = "lean-ctx-badge";
  document.body.appendChild(badge);
  return badge;
}

function showCompressing() {
  const b = createBadge();
  b.textContent = "lean-ctx: compressing...";
  b.classList.add("visible");
}

function hideCompressing() {
  const b = createBadge();
  b.classList.remove("visible");
}

function showSavings(inputTokens, outputTokens, savings) {
  const b = createBadge();
  b.textContent = `lean-ctx: ${inputTokens}\u2192${outputTokens} tok (-${savings.toFixed(0)}%)`;
  b.classList.add("visible");
  setTimeout(() => b.classList.remove("visible"), 4000);
}

function updateStats(response) {
  chrome.storage.local.get(["stats"], (result) => {
    const stats = result.stats || { totalSaved: 0, totalCommands: 0 };
    stats.totalSaved += (response.inputTokens || 0) - (response.outputTokens || 0);
    stats.totalCommands += 1;
    chrome.storage.local.set({ stats });
  });
}

function loadSettings() {
  chrome.runtime.sendMessage({ action: "getSettings" }, (s) => {
    if (chrome.runtime.lastError) return;
    if (s) extensionSettings = { ...extensionSettings, ...s };
  });
  chrome.storage.onChanged.addListener((changes) => {
    if (changes.settings?.newValue) {
      extensionSettings = { ...extensionSettings, ...changes.settings.newValue };
    }
  });
}

function init() {
  const config = getSiteConfig();
  if (!config) return;

  loadSettings();

  const observer = new MutationObserver(() => {
    const inputs = document.querySelectorAll(config.input);
    inputs.forEach((input) => hookInput(input, config));
  });

  observer.observe(document.body, { childList: true, subtree: true });

  const inputs = document.querySelectorAll(config.input);
  inputs.forEach((input) => hookInput(input, config));

  hookSendButton(config);
}

init();
