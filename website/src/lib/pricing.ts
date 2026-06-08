/**
 * Pricing SSOT for the commercial Team / Cloud plane.
 *
 * Mirrors the canonical plan entitlements in the engine
 * (`rust/src/core/billing/plans.rs`, contract `billing-plane-v1`). Every local
 * feature is free forever — these tiers only add team coordination, hosting,
 * scale and governance (the Local-Free Invariant). Display prices match the
 * Stripe catalog provisioned by `lean-ctx-cloud/cloud-infra/stripe-setup.py`
 * ($19/month or $190/year for Team).
 *
 * The account dashboard reads live entitlements from
 * `GET /api/account/entitlements`; this module only drives the marketing copy
 * on the pricing page, so it never has to be perfectly in sync with a user's
 * actual subscription — it describes what each tier grants.
 */

export const SALES_EMAIL = 'hello@leanctx.com';

/** Wire id of a self-serve checkout plan (matches `Plan::as_str` in the engine). */
export type CheckoutPlan = 'team';

export interface PricingTier {
  /** Stable id; matches the engine's `Plan` wire ids. */
  id: 'free' | 'team' | 'enterprise';
  name: string;
  tagline: string;
  /** Display price for the default (monthly) cadence, e.g. "$19" or "Custom". */
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
   * What this tier adds, top to bottom. The first entry of a paid tier is the
   * "everything in <lower tier>" carry-over.
   */
  features: string[];
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
    id: 'team',
    name: 'Team',
    tagline: 'One shared, hosted context for your whole team — and its agents.',
    priceMonthly: '$19',
    priceYearly: '$190',
    unit: '/month',
    priceNote: 'billed monthly · or $190/year (save ~17%)',
    featured: true,
    cta: { label: 'Start with Team', kind: 'checkout', href: '/account/billing/?upgrade=team' },
    features: [
      'Everything in Free, for your whole team',
      'Up to 25 seats on one shared workspace',
      '5 GB hosted retrieval index (BM25 + graph + artifacts)',
      '5 managed connectors (GitHub, GitLab, Jira, Postgres …)',
      'Private extension & persona registry',
      '90-day audit log retention',
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
      'Everything in Team, without the ceilings',
      'Unlimited seats, hosted index & connectors',
      'SSO + SCIM provisioning',
      '10-year audit log retention',
      'Self-host the audited team server on your own cloud',
      'Dedicated support & SLA',
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
    q: 'What exactly does a paid plan add?',
    a: 'A shared, hosted context plane: one retrieval index your whole team and your CI agents query, managed connectors to your tools, a private registry, audit retention and SSO/SCIM at the top. It is additive — your local setup keeps working exactly as before.',
  },
  {
    q: 'How is Team billed?',
    a: 'A flat $19/month (or $190/year) for the workspace, including up to 25 seats. You can upgrade, switch cadence or cancel any time from the billing portal — cancellations take effect at the end of the period.',
  },
  {
    q: 'Can we self-host instead?',
    a: 'Yes. The audited team server is Apache-2.0 and self-hosting is a free capability — see the “Self-host a team server” journey. Enterprise adds SSO/SCIM, unlimited scale and a support SLA on top.',
  },
];
