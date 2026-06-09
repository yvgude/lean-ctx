/**
 * Pure, framework-free ROI estimator for the pricing page.
 *
 * It answers one question a buyer actually has: *"roughly how much does lean-ctx
 * save my team in LLM token costs?"* The savings come from the **free, local**
 * engine (the Local-Free Invariant) — they are not gated behind a plan. We expose
 * the comparison to a Team subscription only to make the point that the paid plane
 * is a rounding error against the savings, never to imply the savings require it.
 *
 * Every number is a transparent, user-adjustable **estimate** derived from the
 * inputs below — there is no hidden data source. Defaults are deliberately
 * conservative (well under lean-ctx's "up to 95%" headline, and savings are
 * request-side estimates, as the tool itself discloses).
 */

export interface RoiInputs {
  /** Developers (and their agents) using lean-ctx. */
  developers: number;
  /** Context tokens processed per developer per working day, in millions. */
  tokensPerDevPerDayM: number;
  /** Blended input price per million tokens, in USD. */
  pricePerMillionUsd: number;
  /** Share of context tokens lean-ctx removes, in percent (0–100). */
  savingsPct: number;
}

export interface RoiResult {
  /** Context tokens lean-ctx removes per month. */
  monthlySavedTokens: number;
  /** Estimated USD saved per month. */
  monthlySavedUsd: number;
  /** Estimated USD saved per year. */
  yearlySavedUsd: number;
  /** A full Team subscription for the same headcount, for comparison. */
  teamMonthlyCostUsd: number;
  /** How many times the monthly savings cover a full Team subscription. */
  roiMultiple: number | null;
}

/** Working days per month used for the monthly projection. */
export const WORKING_DAYS_PER_MONTH = 21;

/** Team list price per seat per month (mirrors `pricing.ts`). */
export const TEAM_SEAT_MONTHLY_USD = 18;

/** Sensible, conservative starting point for the interactive calculator. */
export const DEFAULT_ROI_INPUTS: RoiInputs = {
  developers: 5,
  tokensPerDevPerDayM: 2,
  pricePerMillionUsd: 3,
  savingsPct: 55,
};

/** Estimate monthly/yearly savings from the inputs. Deterministic and DOM-free. */
export function computeRoi(inputs: RoiInputs): RoiResult {
  const developers = clampInt(inputs.developers, 1);
  const tokensPerDay = clampNum(inputs.tokensPerDevPerDayM) * 1_000_000;
  const price = clampNum(inputs.pricePerMillionUsd);
  const fraction = Math.min(1, Math.max(0, clampNum(inputs.savingsPct) / 100));

  const dailyTokens = developers * tokensPerDay;
  const savedTokensPerDay = dailyTokens * fraction;
  const monthlySavedTokens = savedTokensPerDay * WORKING_DAYS_PER_MONTH;
  const monthlySavedUsd = (monthlySavedTokens / 1_000_000) * price;
  const yearlySavedUsd = monthlySavedUsd * 12;

  const teamMonthlyCostUsd = developers * TEAM_SEAT_MONTHLY_USD;
  const roiMultiple = teamMonthlyCostUsd > 0 ? monthlySavedUsd / teamMonthlyCostUsd : null;

  return {
    monthlySavedTokens,
    monthlySavedUsd,
    yearlySavedUsd,
    teamMonthlyCostUsd,
    roiMultiple,
  };
}

function clampNum(n: number): number {
  return Number.isFinite(n) && n > 0 ? n : 0;
}

function clampInt(n: number, min: number): number {
  return Number.isFinite(n) && n >= min ? Math.floor(n) : min;
}
