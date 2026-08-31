/**
 * One-time design-partner and SDK invitation for the local dashboard.
 * The campaign key is versioned so future invitations can be shown once.
 */
(function () {
  'use strict';

  var STORAGE_KEY = 'leanctx_partner_sdk_promo_v1';
  var OVERLAY_ID = 'leanctxPartnerPromo';
  var previousFocus = null;
  var backgroundState = [];
  var blockingObserver = null;
  var openFrame = null;

  function readStorage(key) {
    try { return localStorage.getItem(key); } catch (_) { return null; }
  }

  function rememberDismissal() {
    try { localStorage.setItem(STORAGE_KEY, 'dismissed'); } catch (_) {}
  }

  function hasBlockingDialog() {
    var onboarding = document.getElementById('onboardOverlay');
    var ownOverlay = document.getElementById(OVERLAY_ID);
    var otherModal = Array.prototype.slice.call(document.querySelectorAll('[aria-modal="true"]'))
      .some(function (dialog) {
        return (!ownOverlay || !ownOverlay.contains(dialog)) && !dialog.closest('[hidden]');
      });
    return !!(
      document.getElementById('lctxTokenGate') ||
      document.querySelector('.tour-overlay') ||
      (onboarding && !onboarding.hidden) ||
      otherModal
    );
  }

  function shouldShow() {
    return readStorage(STORAGE_KEY) !== 'dismissed' &&
      readStorage('lctx_onboarded') === '1' &&
      !hasBlockingDialog();
  }

  function setBackgroundInert(overlay) {
    backgroundState = Array.prototype.slice.call(document.body.children)
      .filter(function (element) { return element !== overlay && element.tagName !== 'SCRIPT'; })
      .map(function (element) {
        var state = {
          element: element,
          hadInert: element.hasAttribute('inert'),
          ariaHidden: element.getAttribute('aria-hidden')
        };
        element.setAttribute('inert', '');
        element.setAttribute('aria-hidden', 'true');
        return state;
      });
  }

  function restoreBackground() {
    backgroundState.forEach(function (state) {
      if (!state.hadInert) state.element.removeAttribute('inert');
      if (state.ariaHidden === null) state.element.removeAttribute('aria-hidden');
      else state.element.setAttribute('aria-hidden', state.ariaHidden);
    });
    backgroundState = [];
  }

  function close(remember) {
    var overlay = document.getElementById(OVERLAY_ID);
    if (remember) rememberDismissal();
    if (!overlay) return;
    if (blockingObserver) blockingObserver.disconnect();
    blockingObserver = null;
    if (openFrame !== null) cancelAnimationFrame(openFrame);
    openFrame = null;
    document.removeEventListener('focusin', keepFocusInDialog, true);
    overlay.classList.remove('show');
    document.body.classList.remove('partner-promo-open');
    setTimeout(function () {
      overlay.remove();
      restoreBackground();
      if (!hasBlockingDialog() && previousFocus && typeof previousFocus.focus === 'function') {
        previousFocus.focus();
      }
      previousFocus = null;
    }, 180);
  }

  function dismiss() { close(true); }

  function focusableElements(dialog) {
    return Array.prototype.slice.call(
      dialog.querySelectorAll('a[href],button:not([disabled]),[tabindex]:not([tabindex="-1"])')
    );
  }

  function handleKeydown(event) {
    var overlay = document.getElementById(OVERLAY_ID);
    if (!overlay) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      dismiss();
      return;
    }
    if (event.key !== 'Tab') return;
    var dialog = overlay.querySelector('[role="dialog"]');
    if (!dialog) return;
    var focusable = focusableElements(dialog);
    if (!focusable.length) return;
    var first = focusable[0];
    var last = focusable[focusable.length - 1];
    if (!dialog.contains(document.activeElement)) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus();
    } else if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  function keepFocusInDialog(event) {
    var overlay = document.getElementById(OVERLAY_ID);
    if (!overlay || overlay.contains(event.target)) return;
    var dialog = overlay.querySelector('[role="dialog"]');
    var focusable = dialog ? focusableElements(dialog) : [];
    if (focusable.length) focusable[0].focus();
  }

  function createOverlay() {
    var overlay = document.createElement('div');
    overlay.id = OVERLAY_ID;
    overlay.className = 'partner-promo-overlay';
    overlay.innerHTML =
      '<section class="partner-promo" role="dialog" aria-modal="true" ' +
        'aria-labelledby="partnerPromoTitle" aria-describedby="partnerPromoDescription">' +
        '<button type="button" class="partner-promo-close" aria-label="Dismiss invitation">&times;</button>' +
        '<div class="partner-promo-kicker">BUILD WITH LEANCTX</div>' +
        '<h2 id="partnerPromoTitle">Take LeanCTX further</h2>' +
        '<p id="partnerPromoDescription" class="partner-promo-intro">' +
          'We are looking for design partners and engineering teams ready to deploy LeanCTX in real workflows.' +
        '</p>' +
        '<div class="partner-promo-options">' +
          '<article class="partner-promo-option">' +
            '<span class="partner-promo-index" aria-hidden="true">01</span>' +
            '<h3>Roll out LeanCTX</h3>' +
            '<p>Work with us on integrations, governance, rollout and measurable context efficiency for your organization.</p>' +
            '<a class="partner-promo-cta partner-promo-cta-primary" ' +
              'href="mailto:yves@thinkery.ch?subject=LeanCTX%20design%20partner%20inquiry">' +
              'Email yves@thinkery.ch <span aria-hidden="true">&rarr;</span></a>' +
          '</article>' +
          '<article class="partner-promo-option">' +
            '<span class="partner-promo-index" aria-hidden="true">02</span>' +
            '<h3>Build your own agent</h3>' +
            '<p>Use the Python SDK with the local LeanCTX Engine to build custom, token-efficient agent workflows.</p>' +
            '<a class="partner-promo-cta" href="https://github.com/Thinkery-AG/leanctx-sdk#readme" ' +
              'target="_blank" rel="noopener noreferrer">Explore the SDK <span aria-hidden="true">&rarr;</span>' +
              '<span class="partner-promo-sr-only"> (opens in a new tab)</span></a>' +
          '</article>' +
        '</div>' +
        '<button type="button" class="partner-promo-later">Dismiss</button>' +
      '</section>';
    return overlay;
  }

  function show() {
    if (!shouldShow() || document.getElementById(OVERLAY_ID)) return false;
    previousFocus = document.activeElement;
    var overlay = createOverlay();
    document.body.appendChild(overlay);
    setBackgroundInert(overlay);
    document.body.classList.add('partner-promo-open');

    overlay.querySelector('.partner-promo-close').addEventListener('click', dismiss);
    overlay.querySelector('.partner-promo-later').addEventListener('click', dismiss);
    overlay.querySelectorAll('.partner-promo-cta').forEach(function (link) {
      link.addEventListener('click', dismiss);
    });
    overlay.addEventListener('click', function (event) {
      if (event.target === overlay) dismiss();
    });
    overlay.addEventListener('keydown', handleKeydown);
    document.addEventListener('focusin', keepFocusInDialog, true);
    if (typeof MutationObserver !== 'undefined') {
      blockingObserver = new MutationObserver(function () {
        if (hasBlockingDialog()) close(false);
      });
      blockingObserver.observe(document.body, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ['class', 'hidden']
      });
    }

    openFrame = requestAnimationFrame(function () {
      openFrame = null;
      if (document.getElementById(OVERLAY_ID) !== overlay) return;
      overlay.classList.add('show');
      overlay.querySelector('.partner-promo-close').focus();
    });
    return true;
  }

  function autoStart() {
    setTimeout(show, 1200);
  }

  window.__leanctxPartnerPromo = {
    dismiss: dismiss,
    shouldShow: shouldShow,
    show: show,
    storageKey: STORAGE_KEY
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', autoStart, { once: true });
  } else {
    autoStart();
  }
})();
