import * as cheerio from "cheerio";

export interface Setup {
  coin: string; direction: "long" | "short";
  entry: number; stopLoss: number; takeProfit: number;
  confidence: number; thesis: string;
}

/** Parses "$6.38" / "6.38" → 6.38; returns NaN if no number. */
function money(text: string | undefined): number {
  if (!text) return NaN;
  const m = text.replace(/,/g, "").match(/-?\d+(\.\d+)?/);
  return m ? Number(m[0]) : NaN;
}

/** "7/10" or "7" → 7; NaN otherwise. */
function confidenceValue(text: string | undefined): number {
  if (!text) return NaN;
  const m = text.match(/(\d{1,2})\s*\/\s*10/) ?? text.match(/(\d{1,2})/);
  return m ? Number(m[1]) : NaN;
}

/** Which Setup field a table header maps to, by its (lower-cased) label text. */
type TableField = "direction" | "entry" | "stopLoss" | "takeProfit" | "confidence";
function fieldForHeader(header: string): TableField | null {
  const h = header.trim().toLowerCase();
  if (h.includes("arah") || h.includes("direction") || h.includes("posisi")) return "direction";
  if (h.includes("entry") || h.includes("masuk")) return "entry";
  if (h.includes("stop") || h === "sl") return "stopLoss";
  if (h.startsWith("tp") || h.includes("take profit") || h.includes("target")) return "takeProfit";
  if (h.includes("keyakinan") || h.includes("confidence") || h.includes("conf")) return "confidence";
  return null;
}

/**
 * Parses Neurobro's expanded "Setup Trading" panel (a markdown table) into a Setup.
 * The coin is supplied by the caller (the loop knows which coin it asked about); it is
 * not present in the panel. Fail-closed: returns null if the setup table is missing or
 * any required field cannot be parsed — never guesses a number.
 */
export function extractSetup(html: string, coin: string): Setup | null {
  const $ = cheerio.load(html);
  const normalizedCoin = coin.trim().toUpperCase();
  if (!normalizedCoin) return null;

  // Find the setup table: the first whose header row maps to our fields (there may be
  // other tables, e.g. token metrics). Require at least direction + entry to qualify.
  let map: Partial<Record<TableField, number>> | null = null;
  let cells: string[] | null = null;
  $("table").each((_, table) => {
    if (map) return; // already found the setup table
    const headers = $(table).find("thead th");
    if (!headers.length) return;
    const candidate: Partial<Record<TableField, number>> = {};
    headers.each((index, th) => {
      const field = fieldForHeader($(th).text());
      if (field) candidate[field] = index;
    });
    if (candidate.direction === undefined || candidate.entry === undefined) return;
    const firstRow = $(table).find("tbody tr").first();
    if (!firstRow.length) return;
    map = candidate;
    cells = firstRow.find("td").map((_, td) => $(td).text().trim()).get();
  });

  if (!map || !cells) return null;
  const resolvedMap: Partial<Record<TableField, number>> = map;
  const resolvedCells: string[] = cells;
  const cellFor = (field: TableField): string | undefined => {
    const index = resolvedMap[field];
    return index === undefined ? undefined : resolvedCells[index];
  };

  const directionText = (cellFor("direction") ?? "").toUpperCase();
  const direction = directionText.includes("LONG") ? "long" : directionText.includes("SHORT") ? "short" : null;
  const entry = money(cellFor("entry"));
  const stopLoss = money(cellFor("stopLoss"));
  const takeProfit = money(cellFor("takeProfit"));
  const confidence = confidenceValue(cellFor("confidence"));

  // Thesis: the "Tesis singkat" blockquote (strip the label prefix), else first paragraph.
  const thesisRaw = ($("blockquote").first().text() || $("p").first().text() || "").trim();
  const thesis = thesisRaw.replace(/^.*?tesis[^:]*:\s*/i, "").trim();

  if (!direction || [entry, stopLoss, takeProfit, confidence].some((n) => !Number.isFinite(n))) {
    return null; // fail-closed
  }
  return { coin: normalizedCoin, direction, entry, stopLoss, takeProfit, confidence, thesis };
}
