/**
 * Pricing SSOT for the commercial Team / Cloud plane.
 *
 * Mirrors the canonical plan entitlements in the engine
 * (`rust/src/core/billing/plans.rs`, contract `billing-plane-v1`). Every local
 * feature is free forever — these tiers only add team coordination, hosting,
 * scale and governance (the Local-Free Invariant). Display prices match the
 * Stripe catalog provisioned by `lean-ctx-cloud/cloud-infra/stripe-setup.py`
 * ($10/seat/month or $100/seat/year for Team billed per seat; $9/month or
 * $90/year for Pro).
 *
 * The account dashboard reads live entitlements from
 * `GET /api/account/entitlements`; this module only drives the marketing copy
 * on the pricing page, so it never has to be perfectly in sync with a user's
 * actual subscription — it describes what each tier grants. Pro adds the hosted
 * Personal Cloud (cross-device sync) at $9/month or $90/year.
 */

export const SALES_EMAIL = 'hello@leanctx.com';

/** Wire id of a self-serve checkout plan (matches `Plan::as_str` in the engine). */
export type CheckoutPlan = 'team' | 'pro' | 'supporter';

/**
 * The voluntary "Supporter" subscription. It funds development and grants
 * account-level recognition only — it never unlocks a feature, so it lives
 * outside the tier ladder (the paid capability tier for individuals is now
 * **Pro**, below). Prices mirror the Stripe catalog provisioned by
 * `stripe-setup.py` ($5/month). Rendered on `/support/` and as a callout under
 * the pricing tiers.
 */
export const supporterOffer = {
  id: 'supporter' as const,
  name: 'Supporter',
  tagline: 'Love LeanCTX? Help fund its development.',
  priceMonthly: '$5',
  unit: '/month',
  priceNote: 'pick any amount · no account needed · cancel anytime',
  href: '/support/',
  perks: [
    'Directly fund the open-source Context OS you rely on every day',
    'No account required — subscribe in seconds, straight through Stripe',
    'Cancel anytime — manage everything from Stripe, no lock-in',
    'Keep LeanCTX independent — no VC, no telemetry, no ads',
  ],
};

/**
 * Open, voluntary Supporter tiers — pick whatever feels right. They all live on
 * the same Stripe "Supporter" product; larger amounts simply give more (the
 * Local-Free Invariant means none of them unlock a feature). Supporting is a flat
 * monthly contribution — there is no annual discount, because people are donating,
 * not buying. Every tier has a no-account Stripe Payment Link that also collects
 * the optional supporters-wall fields (display name, message, opt-in). Prices and
 * links are provisioned idempotently by `stripe-setup.py --supporter-links`.
 */
export interface SupporterTier {
  id: 'base' | 'plus' | 'pro';
  /** Short recognition label shown under the amount. */
  label: string;
  /** Display price (whole USD). */
  monthly: string;
  /** Monthly amount in whole USD — drives selector ordering. */
  amount: number;
  /** Highlighted as the suggested amount. */
  featured?: boolean;
  /** Direct, account-free monthly Stripe Payment Link. */
  links: { monthly: string };
}

export const supporterTiers: SupporterTier[] = [
  {
    id: 'base',
    label: 'Supporter',
    monthly: '$5',
    amount: 5,
    links: {
      monthly: 'https://buy.stripe.com/4gM9AN0x04JcagU2AtcjS02',
    },
  },
  {
    id: 'plus',
    label: 'Backer',
    monthly: '$10',
    amount: 10,
    featured: true,
    links: {
      monthly: 'https://buy.stripe.com/cNi28lgvYdfI2Os4IBcjS04',
    },
  },
  {
    id: 'pro',
    label: 'Sponsor',
    monthly: '$25',
    amount: 25,
    links: {
      monthly: 'https://buy.stripe.com/eVq28l4Ng5NgbkYcb3cjS06',
    },
  },
];

export interface PricingTier {
  /** Stable id; matches the engine's `Plan` wire ids. */
  id: 'free' | 'pro' | 'team' | 'enterprise';
  name: string;
  tagline: string;
  /** Display price for the default (monthly) cadence, e.g. "$10" or "Custom". */
  priceMonthly: string;
  /** Display price for the yearly cadence, when the tier is self-serve. */
  priceYearly?: string;
  /** Unit shown next to the price, e.g. "/month" or "forever". */
  unit?: string;
  /** Small line under the price (yearly equivalent / billing note). */
  priceNote?: string;
  /** Visually highlighted as the recommended tier. */
  featured?: boolean;
  cta: {
    label: string;
    /**
     * `checkout` → start a Stripe Checkout session via the account dashboard.
     * `install`  → send the visitor to the install guide.
     * `contact`  → open a sales conversation.
     */
    kind: 'checkout' | 'install' | 'contact';
    href: string;
  };
  /**
   * What this tier adds, top to bottom — **only capabilities that ship today**.
   * The first entry of a paid tier is the "everything in <lower tier>"
   * carry-over. Anything not yet delivered belongs in [`roadmap`](Self.roadmap),
   * never here (no-vaporware rule).
   */
  features: string[];
  /**
   * Capabilities that are announced but not yet generally available. Rendered
   * under a distinct "On the roadmap" heading so buyers can tell apart what they
   * get now from what is coming — we never bill for these as if they shipped.
   */
  roadmap?: string[];
}

export const pricingTiers: PricingTier[] = [
  {
    id: 'free',
    name: 'Free',
    tagline: 'The complete local Context OS. No account, no ceilings on local use.',
    priceMonthly: '$0',
    unit: 'forever',
    cta: { label: 'Install LeanCTX', kind: 'install', href: '/docs/getting-started/' },
    features: [
      'Every local feature — reads, search, AST for 18 languages, 95+ shell patterns',
      'All MCP tools, 10 read modes, Context Personas, plugins & WASM',
      'Runs fully offline — your code never leaves your machine',
      'One developer · Apache-2.0 open source',
    ],
  },
  {
    id: 'pro',
    name: 'Pro',
    tagline: 'Your Personal Cloud — your own context, synced across every device.',
    priceMonthly: '$9',
    priceYearly: '$90',
    unit: '/month',
    priceNote: 'one account · or $90/year (save ~17%) · cancel anytime',
    cta: { label: 'Upgrade to Pro', kind: 'checkout', href: '/account/billing/?upgrade=pro' },
    features: [
      'Everything in Free, plus your hosted Personal Cloud',
      'Cross-device sync & backup of your own learned context',
      'Knowledge & memory synced across every machine you work on',
      'Learned shell patterns, CEP score history & gotchas, synced',
      'Buddy state and feedback thresholds follow you everywhere',
      'Supporter badge — keep LeanCTX independent',
      'One account, single seat · manage or cancel anytime via Stripe',
    ],
  },
  {
    id: 'team',
    name: 'Team',
    tagline: 'One shared, audited context for your whole team — and its agents.',
    priceMonthly: '$10',
    priceYearly: '$100',
    unit: '/seat/month',
    priceNote: 'per seat · choose 1–25 seats at checkout · or $100/seat/year (save ~17%)',
    featured: true,
    cta: { label: 'Start with Team', kind: 'checkout', href: '/account/billing/?upgrade=team' },
    features: [
      'Everything in Free, for your whole team',
      'One shared workspace your team — and its CI agents — query',
      'Role-based access: viewer · member · admin · owner',
      'Full audit log of every context access',
      'Live shared event stream across your agents',
      'BM25 + graph + artifact retrieval over your code',
      '1–25 seats, billed per seat — add or remove anytime (prorated)',
      'Your private team server, provisioned the moment you check out',
    ],
    roadmap: [
      'Self-serve hosted index with a 5 GB quota & usage dashboard',
      'Managed connectors (GitHub, GitLab, Jira, Postgres …)',
      'Private extension & persona registry',
      'Marketplace revenue share for your authors',
    ],
  },
  {
    id: 'enterprise',
    name: 'Enterprise',
    tagline: 'Governance, scale and compliance for the whole organisation.',
    priceMonthly: 'Custom',
    cta: {
      label: 'Talk to us',
      kind: 'contact',
      href: `mailto:${SALES_EMAIL}?subject=LeanCTX%20Enterprise`,
    },
    features: [
      'Everything in Team, for your whole organisation',
      'Self-host the audited team server on your own cloud',
      'Negotiated scale: seats, retrieval index & connectors',
      'Dedicated onboarding, priority support & SLA',
    ],
    roadmap: [
      'SSO + SCIM provisioning',
      '10-year audit log retention & compliance exports',
    ],
  },
];

/** The three pricing FAQ entries shown under the tiers. */
export const pricingFaq: Array<{ q: string; a: string }> = [
  {
    q: 'Is the local tool really free?',
    a: 'Yes — forever. The entire local engine (reads, search, AST, shell compression, MCP tools, read modes, personas, plugins) is Apache-2.0 and runs without an account. Paid plans never gate a local feature; they only add team coordination, hosting, scale and governance.',
  },
  {
    q: 'What is Pro, and how is it different from Supporter?',
    a: 'Pro ($9/month or $90/year) is your Personal Cloud: a hosted, account-bound service that syncs and backs up your own learned context — knowledge & memory, shell patterns, CEP scores, gotchas and more — across every device you work on. Supporter is a voluntary tip that funds development and grants recognition only (a badge), never a capability. Pro is for an individual who wants their own context everywhere; Team is for sharing one audited context across people.',
  },
  {
    q: 'What exactly does a paid plan add?',
    a: 'A shared, audited context plane: one workspace your whole team and your CI agents query, with role-based access (viewer / member / admin / owner), a full audit log of every access and a live shared event stream. It is additive — your local setup keeps working exactly as before. Managed connectors, a private registry, marketplace revenue share and SSO/SCIM are on the roadmap and labelled as such on each plan.',
  },
  {
    q: 'Is the hosted team server available right now?',
    a: 'Yes. The team server is production-ready and you can self-host it for free (Apache-2.0). Prefer managed hosting? The moment your Team checkout completes we provision your private team server for you — then you manage everything (server URL, owner token, member seats with role-based access) yourself from your account dashboard under Account → Team.',
  },
  {
    q: 'How is Team billed?',
    a: 'Per seat — $10/seat/month (or $100/seat/year, ~17% off). You pick how many seats you need (1–25) at checkout; add or remove seats anytime from the billing portal and Stripe automatically prorates the difference. Switch cadence or cancel any time — cancellations take effect at the end of the period.',
  },
  {
    q: 'Can we self-host instead?',
    a: 'Yes. The audited team server is Apache-2.0 and self-hosting is a free capability — see the “Self-host a team server” journey. Enterprise adds SSO/SCIM, unlimited scale and a support SLA on top.',
  },
];
