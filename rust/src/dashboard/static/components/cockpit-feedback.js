/**
 * Product feedback — four questions about how people actually use lean-ctx.
 *
 * Three of them are free text on purpose: a fixed list of features would only
 * ever return the answers it already contained, and the point is to learn which
 * use cases exist, not to confirm the ones we guessed. The one multiple-choice
 * question is frequency, which is what makes the free text groupable later —
 * "what's missing" from a daily user and from someone who installed it
 * yesterday are usually different wishes.
 *
 * Nothing is sent unless the button is pressed. There is no background caller
 * for POST /api/feedback, the form starts empty on every load, and the notice
 * above the button names where the answers go before they go there.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function esc(value) {
  return String(value == null ? '' : value).replace(/[&<>"']/g, function (ch) {
    return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch];
  });
}

/* Kept in step with the server cap; the counter turns red before the send does. */
var MAX_ANSWER = 2000;

var FREQUENCIES = [
  { id: 'daily', label: 'Daily' },
  { id: 'weekly', label: 'A few times a week' },
  { id: 'occasionally', label: 'Occasionally' },
  { id: 'new', label: 'Just started' },
];

var QUESTIONS = [
  {
    id: 'use_case',
    label: 'What do you use lean-ctx for?',
    hint: 'The work it does for you — in your words.',
  },
  {
    id: 'likes_most',
    label: 'What do you like most about it?',
    hint: 'A tool, a mode, a behaviour — whatever you would miss first.',
  },
  {
    id: 'wishes',
    label: 'What is missing?',
    hint: 'The feature you keep wishing were there.',
  },
];

class CockpitFeedback extends HTMLElement {
  constructor() {
    super();
    this._answers = { use_case: '', likes_most: '', wishes: '', contact: '' };
    this._frequency = '';
    this._notice = null;
    this._sending = false;
    this._sent = false;
  }

  connectedCallback() {
    this.render();
  }

  _setNotice(kind, msg) {
    this._notice = { kind: kind, msg: msg };
    this.render();
  }

  async _send() {
    if (this._sending) return;

    var answered = ['use_case', 'likes_most', 'wishes'].some(
      function (id) { return this._answers[id].trim() !== ''; },
      this
    );
    if (!answered) {
      this._setNotice('err', 'Answer at least one question before sending.');
      return;
    }

    var apiFetch = api();
    if (!apiFetch) {
      this._setNotice('err', 'Dashboard API unavailable — reload the page.');
      return;
    }

    this._sending = true;
    this.render();

    try {
      var res = await apiFetch('/api/feedback', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          use_case: this._answers.use_case,
          likes_most: this._answers.likes_most,
          wishes: this._answers.wishes,
          frequency: this._frequency,
          contact: this._answers.contact,
        }),
      });
      var data = null;
      try {
        data = await res.json();
      } catch (e) {
        data = null;
      }

      if (res && res.ok) {
        // Only now is the typed text cleared — an error must never lose it.
        this._sent = true;
        this._answers = { use_case: '', likes_most: '', wishes: '', contact: '' };
        this._frequency = '';
        this._notice = { kind: 'ok', msg: 'Sent — thank you.' };
      } else {
        var reason = (data && (data.error || data.message)) || 'the server refused it';
        this._notice = { kind: 'err', msg: 'Not sent: ' + reason };
      }
    } catch (e) {
      this._notice = { kind: 'err', msg: 'Not sent: ' + (e && e.message ? e.message : 'network error') };
    } finally {
      this._sending = false;
      this.render();
    }
  }

  _bind() {
    var self = this;

    QUESTIONS.concat([{ id: 'contact' }]).forEach(function (q) {
      var el = self.querySelector('#fb-' + q.id);
      if (!el) return;
      el.addEventListener('input', function () {
        self._answers[q.id] = el.value;
        var counter = self.querySelector('#fb-count-' + q.id);
        if (counter) {
          var n = el.value.length;
          counter.textContent = n + ' / ' + MAX_ANSWER;
          counter.style.color = n > MAX_ANSWER ? 'var(--red)' : '';
        }
      });
    });

    Array.prototype.forEach.call(self.querySelectorAll('[data-freq]'), function (btn) {
      btn.addEventListener('click', function () {
        var id = btn.getAttribute('data-freq');
        // Clicking the active choice clears it: the question is optional and
        // there is otherwise no way back to "I would rather not say".
        self._frequency = self._frequency === id ? '' : id;
        self.render();
      });
    });

    var send = self.querySelector('#fb-send');
    if (send) send.addEventListener('click', function () { self._send(); });

    var again = self.querySelector('#fb-again');
    if (again) {
      again.addEventListener('click', function () {
        self._sent = false;
        self._notice = null;
        self.render();
      });
    }
  }

  render() {
    if (this._sent) {
      this.innerHTML =
        '<div class="card">' +
        '<div class="card-header"><h3>Thank you</h3></div>' +
        '<p class="hs" style="margin:0 0 12px;font-size:12px;opacity:.85">' +
        'Your answers are with us. They shape what gets built next.</p>' +
        '<button type="button" id="fb-again" class="filter-btn">Send more</button>' +
        '</div>';
      this._bind();
      return;
    }

    var self = this;
    var fields = QUESTIONS.map(function (q) {
      var value = self._answers[q.id];
      return (
        '<div style="margin-bottom:16px">' +
        '<label for="fb-' + q.id + '" class="hs" ' +
        'style="display:block;font-size:12px;font-weight:600;margin-bottom:2px">' +
        esc(q.label) + '</label>' +
        '<p class="hs" style="margin:0 0 6px;font-size:11px;opacity:.7">' + esc(q.hint) + '</p>' +
        '<textarea id="fb-' + q.id + '" rows="3" ' +
        'style="width:100%;background:var(--bg);color:inherit;border:1px solid var(--border);' +
        'border-radius:6px;padding:8px 10px;font:inherit;font-size:12px;resize:vertical">' +
        esc(value) + '</textarea>' +
        '<span id="fb-count-' + q.id + '" class="hs" ' +
        'style="display:block;text-align:right;font-size:10px;opacity:.55;margin-top:2px">' +
        value.length + ' / ' + MAX_ANSWER + '</span>' +
        '</div>'
      );
    }).join('');

    var freqButtons = FREQUENCIES.map(function (f) {
      var on = self._frequency === f.id;
      return (
        '<button type="button" class="filter-btn' + (on ? ' active' : '') + '" ' +
        'data-freq="' + esc(f.id) + '" aria-pressed="' + (on ? 'true' : 'false') + '">' +
        esc(f.label) + '</button>'
      );
    }).join('');

    var notice = '';
    if (this._notice) {
      var color = this._notice.kind === 'ok' ? 'var(--green)' : 'var(--red)';
      notice =
        '<p class="hs" style="margin:10px 0 0;font-size:11px;color:' + color + '">' +
        esc(this._notice.msg) + '</p>';
    }

    this.innerHTML =
      '<div class="card">' +
      '<div class="card-header"><h3>Tell us how you use lean-ctx</h3></div>' +
      '<p class="hs" style="margin:0 0 14px;font-size:12px;opacity:.8">' +
      'Four questions, all optional. What you write here decides what gets built next.</p>' +
      fields +
      '<div style="margin-bottom:16px">' +
      '<span class="hs" style="display:block;font-size:12px;font-weight:600;margin-bottom:6px">' +
      'How often do you use it?</span>' +
      '<div class="filter-row" style="display:flex;gap:6px;flex-wrap:wrap">' + freqButtons + '</div>' +
      '</div>' +
      '<div style="margin-bottom:16px">' +
      '<label for="fb-contact" class="hs" ' +
      'style="display:block;font-size:12px;font-weight:600;margin-bottom:2px">' +
      'Email or handle <span style="opacity:.6;font-weight:400">(optional)</span></label>' +
      '<p class="hs" style="margin:0 0 6px;font-size:11px;opacity:.7">' +
      'Only if you want an answer back. Leave it empty to stay anonymous.</p>' +
      '<input id="fb-contact" type="text" autocomplete="off" ' +
      'value="' + esc(this._answers.contact) + '" ' +
      'style="width:100%;background:var(--bg);color:inherit;border:1px solid var(--border);' +
      'border-radius:6px;padding:8px 10px;font:inherit;font-size:12px">' +
      '</div>' +
      // Said before the button, not after: pressing send is the moment
      // something leaves this machine, and the person deserves to know that
      // before they press it rather than in a changelog.
      '<p class="hs" style="margin:0 0 10px;font-size:11px;opacity:.7">' +
      'Pressing send transmits these answers to leanctx.com, together with your ' +
      'lean-ctx version and the anonymous installation id. Nothing else — no code, ' +
      'paths, prompts or usage data — and nothing at all until you press it.</p>' +
      '<button type="button" id="fb-send" class="filter-btn active"' +
      (this._sending ? ' disabled' : '') + '>' +
      (this._sending ? 'Sending…' : 'Send feedback') + '</button>' +
      notice +
      '</div>';

    this._bind();
  }
}

customElements.define('cockpit-feedback', CockpitFeedback);
