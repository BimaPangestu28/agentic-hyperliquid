import * as cheerio from "cheerio";

export interface Setup {
  coin: string; direction: "long" | "short";
  entry: number; stopLoss: number; takeProfit: number;
  confidence: number; thesis: string;
}

/** Parses "$69.32" / "69.32" → 69.32; returns NaN if no number. */
function money(text: string | undefined): number {
  if (!text) return NaN;
  const m = text.replace(/,/g, "").match(/-?\d+(\.\d+)?/);
  return m ? Number(m[0]) : NaN;
}

/** Finds the `$`-price text in the row that contains a label span equal to `label`. */
function priceForLabel($: cheerio.CheerioAPI, label: string): number {
  const labelSpan = $("span").filter((_, el) => $(el).text().trim() === label).first();
  if (!labelSpan.length) return NaN;
  const row = labelSpan.closest("div.flex-shrink-0");
  const price = row.find("span").filter((_, el) => $(el).text().trim().startsWith("$")).first();
  return money(price.text());
}

export function extractSetup(html: string): Setup | null {
  const $ = cheerio.load(html);

  // Must be a setup card.
  const heading = $("h4").filter((_, el) => $(el).text().includes("Setup trading untuk")).first();
  if (!heading.length) return null;
  const coin = heading.text().replace(/.*untuk\s+/i, "").trim().toUpperCase();
  if (!coin) return null;

  // Direction: the "Arah" card's bold value.
  const arahLabel = $("div").filter((_, el) => $(el).text().trim() === "Arah").first();
  const arahValue = arahLabel.closest("[data-slot='card']").find(".font-bold").text().toUpperCase();
  const direction = arahValue.includes("LONG") ? "long" : arahValue.includes("SHORT") ? "short" : null;

  // Confidence: first "{n}/10".
  const confMatch = $.root().text().match(/(\d{1,2})\s*\/\s*10/);
  const confidence = confMatch ? Number(confMatch[1]) : NaN;

  const entry = priceForLabel($, "Masuk");
  const stopLoss = priceForLabel($, "SL");
  const takeProfit = priceForLabel($, "TP1");
  const thesis = $("p").filter((_, el) => $(el).text().includes("$") || $(el).text().length > 20).first().text().trim();

  if (!direction || [entry, stopLoss, takeProfit, confidence].some((n) => !Number.isFinite(n))) {
    return null; // fail-closed
  }
  return { coin, direction, entry, stopLoss, takeProfit, confidence, thesis };
}
