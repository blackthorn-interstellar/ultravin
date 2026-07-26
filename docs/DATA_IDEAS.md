# Car-data integration ideas

Brainstorm output, 2026-07-25. **Nothing here is implemented or committed to** — this is the
exhaustive idea backlog, produced by a multi-agent sweep (7 research lenses + 3 gap-fill lenses →
merge/dedupe → adversarial fact-check, with web verification of sources and licenses; several
verifiers downloaded and parsed the actual datasets).

The lens for every idea: ultravin is a **fully offline** decoder with data baked into the wheel,
refreshed monthly by automation, MIT-distributed. So a great dataset (1) joins on VIN / VIN
pattern / WMI / make+model+year, (2) is legally redistributable (public domain or open license),
(3) fits in single-digit MB embedded, (4) has a bulk source the monthly refresh can poll.
Ideas that fail a criterion are still listed — with the failure stated honestly.

⭐ = top candidate. All 13 top candidates plus HLDI got a dedicated adversarial fact-check
(dataset downloaded or API probed, license text located); verdicts are inline in the catalog and
summarized in the table below. The three gap-lens sections (FMCSA, ADAS, auto finance) were
researched with live source verification but did not go through the second adversarial pass.

---

## The short version

**The single best move is the NHTSA ODI safety stack**: recalls + completion rates +
investigations + complaint aggregates + TSB index. All five are US-government public domain with
daily-regenerated static bulk zips, and they share one engineering cost — a free-text make/model
normalization layer against vPIC's controlled vocabulary. They change the product from "what is
this car" to "what is wrong with this car." Verification's best surprise: **84.7% of complaint
records carry an 11-character VIN prefix** — exactly the prefix ultravin decodes — so complaints
join at VIN-pattern precision, cleaner than make/model text (4/5 value, 5/5 feasibility, 2.5 MB
embedded).

**Second: the EPA/DOE fueleconomy.gov label pack.** ~2 MB zipped, public domain, verified live.
MPG/MPGe, EV range, fuel cost, CO2, plus cargo volumes and turbo flags vPIC lacks. The catch
(measured empirically): ~19% of rows stay ambiguous after all numeric disambiguators, so the API
must return candidate sets or ranges, never fake trim-exact numbers.

**The second-round standouts** (from the gap lenses):

- **NHTSA SGO ADAS/ADS crash packs** — every reported crash with a Level-2 system (Autopilot,
  BlueCruise…) or robotaxi ADS engaged. Public domain, ~1.5 MB gzipped including archive,
  monthly bulk CSVs, and **100% of rows carry an 11-char VIN prefix**. Plus the derived
  `adas-fingerprint` idea: which named L2 system ships on which VIN pattern — data nobody
  publishes.
- **SEC ABS-EE auto-securitization aggregates** — millions of loan/lease-level filings on EDGAR
  (free, no use restrictions) yield real-transaction depreciation curves, per-model default and
  repossession-recovery rates, and lease residual-accuracy — the data ALG/Black Book/Manheim
  sell. Harvest is heavy (GBs per month) but runs in the pipeline; embedded aggregates are 1–3 MB.
- **FMCSA truck stack** — 13.7M roadside inspections and 5M CMV crash records carry full VINs.
  Dogfooding angle: batch-decode them with ultravin itself, then ship per-model out-of-service
  rates, component failure fingerprints, and a "has this truck been in a reportable wreck" bloom
  filter. The used-truck market has no free equivalent.

**Third: the zero-data derived features.** Odometer-disclosure-exemption flag (pure rule on
fields already decoded), clone-sniff heuristics, serial-ceiling production floors, fleet-package
codes — near-zero bytes, no licensing risk, and nobody else has them.

**Sleeper hits:** the Kia/Hyundai immobilizer flag (<10 KB, arguably the most useful theft warning
a 2026 decoder can print), the Cash-for-Clunkers scrapped-VIN set (677k federally-destroyed VINs
as an offline clone detector — verified, ~2–3 MB), and the WA EV registration census — whose real
value turned out to be internal: a free monthly ~290k-row ground-truth corpus for validating the
decoder in CI, no embedding needed.

**Documented no-gos** (kept so nobody re-litigates them): **HLDI insurance-loss data** (verified
hard no — reports carry explicit "COPYRIGHTED DOCUMENT, DISTRIBUTION RESTRICTED" notices requiring
written permission; only paths are asking HLDI or a user-side PDF fetcher), salvage-auction
presence (Copart/IAAI — ToS-prohibited, no lawful bulk), NICB VINCheck (link-out only), Verisk
VINMASTER (enterprise contracts), Euro NCAP (copyright forbids redistribution), wheel/tire fitment
and OEM maintenance schedules (commercial moats with no open source — a from-OEM-PDF community
extraction is the only clean path).

---

## Fact-check verdicts

| Idea | Value | Feas. | What verification changed |
|---|---|---|---|
| NHTSA complaint aggregates | 4/5 | 5/5 | Upgraded: field 15 is an 11-char VIN prefix on 84.7% of records → direct VIN-pattern joins. Must dedupe by ODINO (rows inflate counts ~39%). Aggregate verified at 2.5 MB zstd. Narratives (1.58 GB) exceed PyPI limits — runtime download only. |
| NHTSA recall campaigns | 4/5 | 4/5 | PD confirmed (`us-pd` on data.gov). ~4.7 MB gz with full narratives. **No VIN ranges in the bulk file** — frame as "campaigns that may apply", never "this VIN has an open recall". Free-text make/model needs an alias map. Bonus: 2025 schema added `DO_NOT_DRIVE` / `PARK_OUTSIDE` flags. |
| fueleconomy.gov labels | 4/5 | 4/5 | PD via EPA data license; 2.1 MB zip verified live (MY1984–2027, 49,995 rows). Join overstated by the brainstorm: heavy model-name normalization required; 18.7% residual ambiguity → return ranges/candidates. |
| TSB / mfr-communications index | 4/5 | 4/5 | PD confirmed (MAP-21 mandated publication). Verified end-to-end: 5.76M rows → 257k unique communications. Real embedded size with summaries is **25–35 MB, ~2× the claim** — ship as an optional `ultravin-tsb` extra. Refresh must list the S3 bucket, not hardcode filenames. |
| HLDI insurance-loss ratings | 4/5 | **1/5** | **Hard no, not a gray area**: per-report "COPYRIGHTED DOCUMENT, DISTRIBUTION RESTRICTED" notices require written permission; MIT redistribution directly conflicts. Data itself is tiny (<200 KB) and compelling. Also: the Hyundai/Kia theft "surge" is stale — WT-24 shows them below average for MY2022–24. |
| NCAP 5-star ratings | 4/5 | 3/5 | PD confirmed, but **no bulk file exists** — acquisition is a ~30k-request polite crawl of a rate-controlled API. No VIN/WMI in records; NCAP model strings embed body styles → candidate-list join, not 1:1. Pre-MY2011 stars aren't comparable to 2011+. |
| Defect investigations | 3/5 | 4/5 | The 4.3 MB zip inflates to 390 MB of denormalized text; after dedupe it's ~1.3 MB gz. Only 29% link to a recall campaign. Ship as a low-cost rider on the recalls stack, not alone. |
| Recall completion rates | 3/5 | 5/5 | Correct URL is `FLAT_RCL_Qrtly_Rpts.zip` + `FLAT_RCL_Annual_Rpts.zip`. **2015+ campaigns only**, figures freeze after the 573.7 reporting window. Per-campaign, not per-VIN. Meaningless without the recalls table; nearly free with it. |
| Transport Canada recalls | 3/5 | 5/5 | OGL-Canada 2.0 confirmed (attribution only, no share-alike). Raw bulk file is **205 MB, not 3–5** — bilingual comment bloat; dedupes to 1.6 MB zstd. 17,893 campaigns fanned to 146k rows. Best shipped inside a combined recalls extra, not standalone. |
| Offline DTC pack | 3/5 | 4/5 | MIT license file is real but data provenance undocumented (community-compiled). Real counts: 18,805 rows / 12,128 unique codes, not "28,220+". Per-make depth is shallow. ~1 MB compressed confirmed. Ship as attributed optional extra. |
| UK MOT reliability atlas | 3/5 | 3/5 | OGL v3 confirmed clean. Raw is ~9.2 GB/yr (3–4× the claim) — crunch needs duckdb/streaming in the pipeline. No VIN, no model year in the extract; UK-market-only labeling mandatory. Still the only free real-world failure-rate data anywhere. |
| NHTSA theft rates (49 CFR 541) | 2/5 | 3/5 | Downgraded: publication **ceased at MY2014** — frozen, 12 years stale, first-year rates only (the Kia/Hyundai epidemic is invisible in it). Some years are TIFF scans needing OCR. Ship only as clearly-labeled historical data, if at all. |
| Cash for Clunkers VINs | 2/5 | 5/5 | Verified end-to-end (file downloaded, 677,048 valid VINs extracted). Sole bulk source is the Internet Archive; pin snapshot + SHA256. ~2–3 MB embedded. Fun + clone-signal, honestly niche. |
| WA EV registration census | 2/5 | 4/5 | ODbL share-alike applies to the shipped extract — would be the first non-PD data in the wheel. Prefix column includes the check digit (counts inflate ~8×); range column junk for 81% of BEVs. **Real prize: free CI ground-truth corpus, zero embedding.** |

---

## Full catalog — 109 ideas


### Safety & recalls

- **NCAP 5-Star Safety Ratings** ⭐ *(obvious)* — **verified 4/5 value, 3/5 feasibility**
  Official NHTSA 5-star crash ratings per vehicle variant since 1990: overall/frontal/side/rollover stars plus rollover risk percentage. No bulk file, so the monthly refresh crawls the API by year/make/model.
  — *Source:* NHTSA SafetyRatings API — https://api.nhtsa.gov/SafetyRatings; public-domain per data.gov (data.transportation.gov/d/jrw6-ye84)
  — *Join:* make+model+year + variant description (mapped to vPIC body class/drive type)
  — *License:* US government work, public domain
  — *Embed:* yes — <1 MB for the entire history · *Size:* <1 MB · *Cadence:* ratings added through the year; monthly API crawl
  — *Why:* The consumer-facing safety datum everyone recognizes, at decode time, offline.
- **NHTSA Defect Investigations** ⭐ *(interesting)* — **verified 3/5 value, 5/5 feasibility**
  All ODI defect investigations (PE/EA/RQ/IR) since 1972: make/model/year, component, open/close dates, summary, and linked recall campaign number. Only 4.3 MB zipped — embeddable in full.
  — *Source:* NHTSA ODI — https://static.nhtsa.gov/odi/ffdd/inv/FLAT_INV.zip (layout INV.txt); verified live
  — *Join:* make+model+year; CAMPNO links to recall campaigns
  — *License:* US government work, public domain
  — *Embed:* yes — ~4 MB zipped, less trimmed · *Size:* ~4 MB zipped · *Cadence:* flat file regenerated daily
  — *Why:* Early-warning signal: an open investigation often precedes a recall by months — cheap to ship, high signal for used-car diligence.
- **NHTSA Owner Complaints (ODI), aggregated** ⭐ *(obvious)* — **verified 4/5 value, 5/5 feasibility**
  ~2M consumer safety complaints since 1995 with crash/fire/injury/death flags and partially-masked VINs; ship an aggregated complaint/crash/fire/injury count table per make+model+year+component (~2-5 MB), with full narratives as an optional extra.
  — *Source:* NHTSA ODI — https://static.nhtsa.gov/odi/ffdd/cmpl/FLAT_CMPL.zip (368 MB full; 5-year chunks available)
  — *Join:* make+model+year for aggregates; records carry user-reported VINs enabling per-pattern rollups
  — *License:* US government work, public domain
  — *Embed:* partial — aggregates yes (~2-5 MB); full narratives (368 MB) as optional extra only · *Size:* ~2-5 MB aggregated · *Cadence:* flat file regenerated daily
  — *Why:* 'This model-year has 412 complaints, 60% powertrain' is a killer reliability signal no offline decoder ships today.
- **NHTSA Recall Campaigns** ⭐ *(obvious)* — **verified 4/5 value, 4/5 feasibility**
  Every US safety recall campaign since 1966 from NHTSA ODI flat files: campaign number, affected make/model/year ranges, component, defect, consequence, remedy, units affected. The single most-requested 'what else about this VIN' feature.
  — *Source:* NHTSA ODI flat files — https://static.nhtsa.gov/odi/ffdd/rcl/FLAT_RCL_POST_2010.zip (+ PRE_2010; layout RCL.txt); verified live
  — *Join:* make+model+year (campaign VIN ranges are not in the bulk file — report open campaigns as 'may apply')
  — *License:* US government work, public domain
  — *Embed:* yes — ~5-8 MB trimmed (22 MB zipped full) · *Size:* ~5-8 MB trimmed · *Cadence:* regenerated daily at source; monthly snapshot fine
  — *Why:* Turns ultravin from 'what is this car' into 'what is wrong with this car' — open-recall flags at decode time, fully offline.
- **Recall completion rates** ⭐ *(interesting)* — **verified 3/5 value, 5/5 feasibility**
  Quarterly manufacturer-reported remedy progress per recall campaign: vehicles involved, remedied, unreachable, removed. Folds into the recalls table as a completion-percentage column for almost no size cost.
  — *Source:* NHTSA ODI — https://static.nhtsa.gov/odi/ffdd/rcl/FLAT_RCL_Qrtly_Rpts.zip (1.5 MB) + Annual_Rpts.zip
  — *Join:* recall campaign number → recalls table → make+model+year
  — *License:* US government work, public domain
  — *Embed:* yes — <2 MB · *Size:* <2 MB · *Cadence:* quarterly filings; flat file regenerated daily
  — *Why:* Upgrades 'recall exists' to 'recall exists and 40% of units are still unfixed — check this one'.
- **Technical Service Bulletins / Manufacturer Communications index** ⭐ *(interesting)* — **verified 4/5 value, 4/5 feasibility**
  Searchable index of every manufacturer communication/TSB filed with NHTSA since 1995: TSB ID, make/model/year, component, date, and a concise fix summary (not the full bulletin PDFs).
  — *Source:* NHTSA ODI — https://static.nhtsa.gov/odi/ffdd/tsbs/TSBS_RECEIVED_*.zip and MFR_COMMS_RECEIVED_*.zip; verified via S3 listing
  — *Join:* make+model+year
  — *License:* US government work, public domain
  — *Embed:* yes — ~10-15 MB trimmed index (~90 MB full set as optional extra) · *Size:* ~10-15 MB trimmed · *Cadence:* regenerated daily; stable 5-year chunk naming
  — *Why:* Mechanics pay Alldata for exactly this index; 'known documented fixes for this model-year' offline is a differentiator.
- **Early Warning Reporting aggregates** *(interesting)*
  Manufacturer-submitted quarterly counts of deaths, injuries, property-damage claims, warranty claims, and field reports by make/model/year. High signal, but no confirmed stable bulk download — the most fragile pipeline on this list.
  — *Source:* NHTSA ODI EWR — https://www.nhtsa.gov/early-warning-reporting/ewr-frequently-asked-questions; extraction via ODI data search/datahub scrape
  — *Join:* make+model+year
  — *License:* US government work, public domain once published; acquisition path is the weak point
  — *Embed:* partial — tables would be ~2-4 MB; blocker is reliable bulk acquisition, not size · *Size:* ~2-4 MB · *Cadence:* quarterly submissions
  — *Why:* Death/injury claim counts per model-year is a harder-edged safety signal than complaints, and almost nobody surfaces it.
- **IIHS crash-test ratings & Top Safety Pick (online adapter)** *(obvious)*
  IIHS crashworthiness, headlight, and front-crash-prevention ratings plus TSP/TSP+ awards via the free registered IIHS API. ToS forbids redistribution, so this ships as an optional online integration with a user-supplied key, not baked-in data.
  — *Source:* IIHS-HLDI API — https://api.iihs.org/ (approval-required, binding display requirements)
  — *Join:* make+model+year (+series/body style; needs fuzzy map to vPIC Series)
  — *License:* Free API but ToS-gated; NOT openly redistributable
  — *Embed:* no — adapter only; a <200 KB MMY→IIHS-series join table could ship for mapping · *Size:* 0 embedded (adapter) · *Cadence:* continuous; annual award refresh
  — *Why:* 'Is this car safe?' is a top-3 post-decode question and IIHS is the industry benchmark.
- **NHTSA NRD crash-test database (injury metrics + measured vehicle parameters)** *(interesting)*
  Engineering records of every NHTSA crash test since the 1970s: impact speed, barrier type, dummy injury metrics (HIC, chest g's, femur loads), plus the measured curb weight, wheelbase, length, and width of each actual test vehicle — the only public-domain source of measured (not brochure) curb weights.
  — *Source:* NHTSA Research & Data — https://nrd.api.nhtsa.dot.gov/ (Vehicle Crash Test DB API; fields CURBWT, WHLBAS, VEHLEN, VEHWID confirmed); catalog.data.gov entry
  — *Join:* make+model+year of tested vehicle (sparse coverage — serve as 'measured reference specs where available')
  — *License:* US government work, public domain
  — *Embed:* yes — metadata + key metrics for ~30k tests distills to ~3-5 MB; signals/photos/video stay out · *Size:* ~3-5 MB · *Cadence:* continuous as tests publish; monthly API pull
  — *Why:* Real HIC numbers behind the stars plus measured curb weights vPIC famously lacks — useful for towing math, shipping, and the engineering-minded.

### Specs & maintenance

- **Offline DTC pack (generic + manufacturer P/B/C/U codes)** ⭐ *(obvious)* — **verified 3/5 value, 4/5 feasibility**
  MIT-licensed lookup table of 28,220+ diagnostic trouble codes — 9,415 generic SAE J2012-style plus 18,805 manufacturer-specific across 33+ brands — mapping P0420/B1342/U0100-style codes to human-readable descriptions.
  — *Source:* Wal33D/dtc-database on GitHub — https://github.com/Wal33D/dtc-database (3.1 MB SQLite + text; MIT verified)
  — *Join:* generic codes universal for 1996+ US VINs (gate on model year); manufacturer codes join on make decoded from the WMI
  — *License:* MIT (verified); caveat: descriptions are community paraphrases, official SAE J2012 text is a paid standard
  — *Embed:* yes — ~1 MB compressed, zero runtime deps · *Size:* ~1 MB compressed · *Cadence:* community repo; git pull monthly
  — *Why:* Decode the VIN, then decode the scan-tool codes for that exact make, fully offline — the most-requested pairing in fleet/telematics pipelines.
- **CarMD repair-cost & maintenance-schedule adapter** *(obvious)*
  By VIN (+mileage): scheduled-maintenance items with parts/labor costs, predicted repairs, and DTC-to-repair mappings via freemium API (10 free credits/day). ToS prohibits caching/redistribution — optional adapter with the user's own key.
  — *Source:* CarMD API — https://www.carmd.com/api/
  — *Join:* full VIN
  — *License:* Commercial freemium; no redistribution
  — *Embed:* no — adapter only · *Size:* 0 (adapter) · *Cadence:* vendor-maintained; live API
  — *Why:* Ownership-cost and maintenance-due answers are the natural next question for fleet and consumer tools after decoding.
- **Factory paint-code cross-reference** *(wild)*
  Cross-reference of factory paint codes to color names (often with PPG/DuPont mix codes) by year/make/model. No clean-license bulk source exists — PaintRef is a hobbyist copyrighted compilation; would need permission or independent recompilation from OEM literature.
  — *Source:* PaintRef.com (no open license); commercial alternates automotivetouchup.com, autocolorlibrary.com
  — *Join:* make+model+year → list of factory paint codes (exact per-VIN color needs the OEM build sheet)
  — *License:* UNCLEAR — legally gray to scrape; flag honestly
  — *Embed:* partial — 1-3 MB and static once obtained, but no clean source today · *Size:* 1-3 MB · *Cadence:* annual (new model-year colors)
  — *Why:* Body shops, touch-up retailers, and restorers constantly bridge VIN→paint code; no open decoder offers it.
- **OBDb per-model extended-PID signalsets** *(interesting)*
  Community-documented vehicle-specific OBD signals: which extended/proprietary PIDs a given make-model-year actually answers — EV state-of-charge, battery health, TPMS pressures, transmission temps — with headers, formulas, and year-range overrides.
  — *Source:* OBDb GitHub org — https://github.com/OBDb (one repo per make-model; browsable at obdb.community, updated daily)
  — *Join:* make+model+model year (repo naming and year-range JSON overrides map cleanly onto the decode)
  — *License:* CC BY-SA 4.0 org-wide — attributed optional extra
  — *Embed:* yes — compiled pack 2-5 MB compressed · *Size:* 2-5 MB compressed · *Cadence:* daily upstream; monthly snapshot
  — *Why:* 'What can I actually read from THIS car?' — no open competitor exists; commercial equivalents are locked inside scan-tool vendors.
- **OEM maintenance schedules, fluid capacities, torque specs — commercial only** *(obvious)*
  Interval-based OEM maintenance schedules, fluid types/capacities, and torque specs — THE most-monetized VIN-adjacent dataset (ALLDATA/Mitchell's core moat), with no open or government alternative verified to exist. The only clean future path is an original community extraction from publicly-posted OEM owner's manual PDFs.
  — *Source:* VehicleDatabases Vehicle Services API — https://vehicledatabases.com/vehicle-services-api; incumbents CarMD/ALLDATA/Mitchell 1; Edmunds' free API discontinued
  — *Join:* full VIN (vendor APIs) or make+model+year+engine
  — *License:* Commercial/proprietary across the board
  — *Embed:* no as licensed data; a from-manuals LLM-extraction pipeline is a project, not a download · *Size:* n/a (cannot ship) · *Cadence:* vendor-managed
  — *Why:* Listed for honesty and roadmap clarity — the clearest candidate for a future community extraction effort.
- **OEM window-sticker / build-sheet fetcher (Monroney module)** *(interesting)*
  OEMs quietly expose original factory window stickers by VIN (Ford 2007+, Stellantis, GM 2012+): full option list, packages, paint, original MSRP. An `ultravin sticker <VIN>` command fetches and parses these — inherently online, user-initiated, no dataset shipped.
  — *Source:* OEM endpoints — e.g. https://www.windowsticker.forddirect.com/windowsticker.pdf?vin={VIN}; Stellantis equipment listing; GM sticker lookups
  — *Join:* full VIN — exact per-vehicle match, the strongest join possible
  — *License:* Undocumented OEM endpoints, no stated license; fine as a user-initiated fetch of their own vehicle's document, could break anytime
  — *Embed:* no — only the endpoint registry and parsers ship (~0 data) · *Size:* ~0 (code only) · *Cadence:* live per-request; endpoint registry checked monthly
  — *Why:* Turns ultravin from spec decoder into as-built decoder: actual factory options, paint, and MSRP — the single most-requested thing generic decoders can't do.
- **SAE J1979 standard PID reference** *(obvious)*
  The standardized OBD-II Mode 01 PIDs (RPM, coolant temp, fuel trim, MAF...) with scaling formulas, units, and byte layouts in validated machine-readable JSON — the universal 'what can I ask any car over the OBD port' table.
  — *Source:* OBDb/SAEJ1979 on GitHub — https://github.com/OBDb/SAEJ1979
  — *Join:* none per-vehicle — universal for 1996+ US-market vehicles; gate on decoded model year and country
  — *License:* CC BY-SA 4.0 — ShareAlike applies; ship as a clearly-attributed optional extra alongside MIT core
  — *Embed:* yes — <100 KB · *Size:* <100 KB · *Cadence:* essentially static
  — *Why:* From VIN decode to 'every standard PID you can poll, with the exact conversion formula' for telematics builders.
- **Tesla option-code & VIN-position enrichment tables** *(interesting)*
  MIT-licensed decode tables for Tesla VIN positions (battery type, motor/drive config, plant, restraint) plus community option-code dictionaries — filling one of vPIC's weakest, highest-lookup-volume makes.
  — *Source:* teslahunt/tesla-vin (MIT, from Tesla service manuals) — https://github.com/teslahunt/tesla-vin; cross-checked vs teslatap.com
  — *Join:* full VIN (Tesla WMIs 5YJ/7SA/LRW/XP7 — positions 4-8 decode directly); option codes when user supplies them
  — *License:* MIT (vendor only the MIT tables, port to Rust); cleanly redistributable
  — *Embed:* yes — <100 KB baked into the binary · *Size:* <100 KB · *Cadence:* sporadic upstream; check monthly
  — *Why:* Battery/motor/plant detail from VIN alone for one of the most-decoded makes, at near-zero size cost.
- **Tow ratings & payload compilation — buildable from OEM PDFs** *(interesting)*
  Max towing capacity, GVWR, curb weight, and payload per trim. Existing compilations (towratings.net, 69,678 configs) carry no open license, but the underlying numbers are uncopyrightable facts extractable from freely-published OEM towing-guide PDFs — a from-scratch extraction ultravin could own.
  — *Source:* OEM annual towing guides (public PDFs); towratings.net etc. free-to-browse but unlicensed compilations
  — *Join:* make+model+trim+year (tow ratings vary by axle ratio/package — pairs with EPA Test Car List axle ratios)
  — *License:* Unclear for compilations; clean if built from OEM PDFs yourself
  — *Embed:* partial — ~2 MB and fully shippable if built from OEM guides; scraping towratings.net is not clean · *Size:* ~2 MB if built · *Cadence:* OEM guides annual per model year
  — *Why:* Towing capacity is a top-five spec question for truck/SUV VINs and vPIC's GVWR class doesn't answer it.
- **VIN→ACES VCdb BaseVehicle bridge (aftermarket Rosetta stone)** *(wild)*
  A precomputed crosswalk from decoded VIN patterns to Auto Care Association VCdb BaseVehicle/Engine IDs — the keys every US parts catalog (ACES fitment) speaks. Sell the bridge, not the data: lawful use requires the customer's own VCdb subscription; the core wheel stays clean.
  — *Source:* Auto Care Association ACES/VCdb — https://www.autocare.org/aces (annual subscription, not redistributable)
  — *Join:* decoded make+model+year+engine → VCdb BaseVehicleID
  — *License:* VCdb subscription-only; crosswalk shippable as a commercial optional extra for VCdb licensees
  — *Embed:* partial — crosswalk is 2-10 MB and offline, gated on customer's VCdb license · *Size:* 2-10 MB crosswalk · *Cadence:* VCdb updates monthly — matches existing automation
  — *Why:* Makes ultravin the default front door to the entire aftermarket parts-catalog ecosystem.
- **Wheel & tire fitment (OE sizes, bolt pattern, offset, placard PSI) — commercial only** *(obvious)*
  OE/plus-size tire dimensions, rim specs, bolt pattern, center bore, offset, and placard pressures for 60,000+ vehicle modifications — the #1 aftermarket question, and a genuine commercial moat: extensive searching found NO open bulk source; Wheel-Size terms forbid extraction, offering only a bespoke self-host license (~$1,570+/yr).
  — *Source:* Wheel-Size.com Fitment API — https://developer.wheel-size.com/; alternates vehicledatabases.com, DriveRightData
  — *Join:* make+model+year+trim (modification names need a map to vPIC trim/series; some vendors take raw VIN)
  — *License:* Commercial only; redistribution forbidden — embed only under a negotiated license, else optional API adapter
  — *Embed:* no as open data; partial (5-20 MB) only under negotiated self-host license — a community 'openfitment' effort is the only clean path · *Size:* 5-20 MB if licensed; 0 as adapter · *Cadence:* vendor continuous
  — *Why:* Bolt pattern and OE tire size top the aftermarket question list (tire shops, wheel retailers, parts pickers) and vPIC has nothing.
- **Wikidata vehicle spec overlay (CC0)** *(wild)*
  Structured crowd-sourced specs for tens of thousands of automobile model/generation items: dimensions, curb mass, platform, assembly plants, production years, and lineage links. Coverage is uneven — a best-effort overlay tier strongest for old, foreign, and exotic vehicles no regulator ever touched.
  — *Source:* Wikidata — https://query.wikidata.org/ SPARQL or JSON dumps
  — *Join:* make+model fuzzy-matched to a Wikidata item, disambiguated by generation via model year vs production period
  — *License:* CC0 — the cleanest license on this list
  — *Embed:* yes — 2-5 MB filtered extract; monthly SPARQL snapshot · *Size:* 2-5 MB · *Cadence:* continuous upstream; monthly snapshot
  — *Why:* Fills gaps for vehicles outside US/EU regulatory reach and adds graph data (platform, lineage) no commercial decoder exposes.
- **opendbc CAN database bundle (VIN → DBC file)** *(wild)*
  MIT-licensed reverse-engineered CAN message definitions (DBC files) for ~300 2016+ vehicles from the comma.ai openpilot ecosystem, including the per-brand platform tables that map model+year to the right DBC.
  — *Source:* commaai/opendbc — https://github.com/commaai/opendbc (MIT verified; per-brand values.py platform mappings)
  — *Join:* make+model+model year → opendbc platform name → DBC file
  — *License:* MIT — clean redistribution; reverse-engineered fidelity caveat, not a legal one
  — *Embed:* yes — 3-8 MB compressed as an optional extra package · *Size:* 3-8 MB compressed · *Cadence:* very active repo; monthly git snapshot
  — *Why:* `ultravin decode <VIN> --dbc` handing you the correct CAN definitions is a genuinely new capability for robotics/telematics/research users.

### EV & emissions

- **EPA/DOE fueleconomy.gov label pack** ⭐ *(obvious)* — **verified 4/5 value, 4/5 feasibility**
  Every model sold since 1984 (~50k records): city/hwy/combined MPG and MPGe, kWh/100mi, EV/PHEV range and charge times, annual fuel cost, CO2 g/mi, GHG score, plus spec fields vPIC lacks — cargo/passenger volumes, turbo/supercharger flags, start-stop, EV motor power. One clean public-domain CSV.
  — *Source:* DOE/EPA — https://www.fueleconomy.gov/feg/epadata/vehicles.csv.zip (2.2 MB zipped, verified) + emissions.csv.zip; index at fueleconomy.gov/feg/download.shtml
  — *Join:* make+model+year disambiguated to trim via displacement/cylinders/transmission/drive — the cleanest fuzzy join available since vPIC decodes all those fields
  — *License:* US government work, public domain; explicitly published for bulk download
  — *Embed:* yes — ~2-5 MB compressed complete · *Size:* ~2-5 MB compressed · *Cadence:* updated several times/year; stable bulk URL fits monthly automation
  — *Why:* MPG, fuel cost, CO2, EV range and charge time at decode time — the highest value-to-byte dataset on this list.
- **Washington EV registration VIN-prefix census** ⭐ *(interesting)* — **verified 2/5 value, 4/5 feasibility**
  ~250k registered WA BEVs/PHEVs with a literal 'VIN (1-10)' column plus model, EV type, electric range, county, and the state's CAFV tax-exemption eligibility call — one of the only government datasets anywhere publishing VIN prefixes, joinable by exact prefix match. Doubles as a monthly ground-truth validation corpus for the decoder itself.
  — *Source:* WA Dept. of Licensing via data.wa.gov — https://data.wa.gov/Transportation/Electric-Vehicle-Population-Data/f6w7-q2d2 (columns API verified)
  — *Join:* VIN pattern — exact match on first 10 characters of a decoded VIN; the strongest join key on this list
  — *License:* ODbL 1.0 per data.gov listing — open with attribution/share-alike obligations on the derived extract; ship the ODbL notice
  — *Embed:* yes — ~1-2 MB aggregated to unique prefixes with counts/county mix · *Size:* ~1-2 MB aggregated · *Cadence:* monthly (last update 2026-07-16)
  — *Why:* '4,812 of this exact VIN pattern registered in WA, top counties, observed range' — real registration ground truth at VIN-prefix level, plus rarity signal and decoder validation.
- **Battery cell supplier & factory provenance** *(wild)*
  A hand-curated mapping of EV model+year+assembly plant to battery cell supplier and cell factory (e.g. 2019-2022 Bolt → LG Ochang), grounded on the factory side by the free NREL/NAATBatt supply chain database. No open bulk source exists — this is editorial curation.
  — *Source:* Compiled from NHTSA/OEM recall notices, IRA FEOC filings, OEM press releases + NREL/NAATBatt Li-Ion Battery Supply Chain Database (free, registration-gated)
  — *Join:* make+model+year + plant city/country from the vPIC decode (plant disambiguates same model, different cells)
  — *License:* Facts table is legally redistributable; comprehensive versions are commercial (Adamas, Benchmark); NAATBatt terms unstated — flag maintenance burden
  — *Embed:* partial — table is tiny (<200 KB); problem is sourcing/maintenance, not size · *Size:* <200 KB · *Cadence:* manual, quarterly-at-best curation
  — *Why:* Explains battery-fire recall exposure, IRA mineral provenance, and chemistry/longevity expectations — the question every used-EV nerd asks and no decoder answers.
- **CAFE manufacturer fuel-economy performance** *(obvious)*
  Manufacturer-by-model-year CAFE standards, achieved fleet MPG, credits/shortfalls, and fines. Weakest join on the list — output is 'the fleet context this vehicle was built into', a make-level attribute only.
  — *Source:* NHTSA CAFE Public Information Center — Excel exports (Socrata datahub entries are link-type, not tabular)
  — *Join:* manufacturer/make + model year + fleet class (car vs light truck) — honest caveat: not per-vehicle
  — *License:* US government work, public domain
  — *Embed:* yes — <1 MB · *Size:* <1 MB · *Cadence:* annual-ish as compliance data finalizes
  — *Why:* Context color for analysts and journalists ('built when Chrysler was paying CAFE fines'); cheap to carry, lowest priority.
- **CARB Executive Order / LEV-ZEV certification index** *(interesting)*
  CARB EO numbers per test group and model year with the California emission category each vehicle certified to (ZEV, TZEV, PZEV, SULEV30, ULEV125...). The authoritative 'is it a CARB-certified ZEV or a 49-state car' record; scrape-only, no bulk file.
  — *Source:* CARB — https://ww2.arb.ca.gov/new-vehicle-and-engine-certification-executive-orders (per-model-year HTML/PDF listings)
  — *Join:* EPA test group ID (via Test Car List bridge) or make+model+year+engine
  — *License:* California public record; freely accessible but no explicit open-data license — redistribution customary but not formally licensed
  — *Embed:* partial — index is ~1-2 MB but requires scraping many per-year pages/PDFs · *Size:* ~1-2 MB · *Cadence:* rolling as EOs issue
  — *Why:* Drives Section-177-state registration questions, HOV-decal history, and ZEV compliance status.
- **EPA Green Vehicle Guide (smog/GHG ratings + certification standard)** *(interesting)*
  Per model+year+engine: EPA Smog Rating (1-10), Greenhouse Gas Rating, SmartWay flag, and the exact emission certification standard string (e.g. 'CA LEV-III SULEV30' vs 'Federal Tier 3 Bin 30') — which reveals whether a vehicle is California-certified or federal-only.
  — *Source:* EPA via fueleconomy.gov downloads — all_alpha_YY CSV files per model year
  — *Join:* make+model+year+engine displacement/cylinders
  — *License:* US government work, public domain; bulk CSVs per model year
  — *Embed:* yes — ~3-5 MB compressed for all years · *Size:* ~3-5 MB · *Cadence:* annual file per model year with mid-year revisions
  — *Why:* CARB-vs-federal certification matters for registering used out-of-state vehicles in Section-177 states; no offline tool answers it.
- **EPA Test Car List (certification test data)** *(interesting)*
  Raw certification records behind the fuel-economy labels since 1979: test group/engine family IDs, equivalent test weight, axle ratio, rated HP, and road-load coastdown coefficients A/B/C — a physics-grade drag/rolling-resistance model of each car, effectively unavailable elsewhere in joinable form.
  — *Source:* EPA — https://www.epa.gov/compliance-and-fuel-economy-data/data-cars-used-testing-fuel-economy (annual CSV/XLSX, 1979-2026; file names carry revision dates so automation scrapes the index)
  — *Join:* make + model year + engine displacement/cylinders + transmission; test-group ID is also the bridge key into CARB Executive Orders
  — *License:* US government work, public domain
  — *Embed:* yes — ~5-20 MB compressed trimmed multi-year subset · *Size:* ~5-20 MB trimmed · *Cadence:* annual per model year with mid-year revisions
  — *Why:* Axle ratios, real test weights, and coastdown coefficients are gold for towing/gearing/tuning users and enable physics-based range estimates at arbitrary speeds.
- **EPA annual certification data (test groups, evap families, emission standards)** *(interesting)*
  Certification records per model: EPA test group / engine family code (the string on the underhood emissions label), evaporative family, and Tier/bin emission standard levels. Lets users confirm a physical car's underhood label matches its VIN decode.
  — *Source:* EPA — https://www.epa.gov/compliance-and-fuel-economy-data/annual-certification-data-vehicles-engines-and-equipment (models file 1.3 MB; test results 76 MB, distill)
  — *Join:* make+model+year; bonus join on the test-group ID printed on the car's own underhood label
  — *License:* US government work, public domain
  — *Embed:* yes — 1-5 MB distilled · *Size:* 1-5 MB distilled · *Cadence:* quarterly
  — *Why:* Smog-check techs and importers constantly need 'what standard is this engine certified to' — currently answered by squinting at a sticker.
- **EV/hybrid battery warranty terms incl. CARB ACC II** *(wild)*
  Per make+model+year: high-voltage battery warranty duration/mileage, capacity-retention guarantees (e.g. Tesla 70%), transferability, and whether CARB ACC II's stricter 70%-at-8yr/100k warranty applies (MY2026+ in ZEV states). Hand-curated from public warranty booklets and regulation text.
  — *Source:* OEM warranty guides + CARB ACC II regulation — https://ww2.arb.ca.gov/our-work/programs/advanced-clean-cars-program; no bulk dataset exists
  — *Join:* make+model+year; ACC II additionally keys off model year >= 2026 (state-of-sale user-supplied)
  — *License:* Compiled facts from public documents — not copyrightable; maintenance burden, not legal risk
  — *Embed:* yes — <100 KB · *Size:* <100 KB · *Cadence:* annual (new model years + CARB schedule)
  — *Why:* For used-EV buyers the battery warranty IS the purchase decision; surfacing it from an offline decode is a feature no competitor has.
- **Emissions defeat-device / consent-decree flag pack** *(wild)*
  A tiny hand-curated table of every vehicle population named in major Clean Air Act defeat-device settlements — VW/Audi/Porsche TDI, RAM/Cummins MY2013-2023 (~960k trucks), FCA EcoDiesel, Daimler BlueTEC — with settlement, remedy type, and recall status.
  — *Source:* EPA/DOJ enforcement pages — e.g. epa.gov/enforcement/volkswagen-clean-air-act-civil-settlement, 2024 Cummins settlement
  — *Join:* make+model+year+engine displacement+fuel type (populations were defined by exactly those attributes)
  — *License:* US government works, public domain; compiled table is facts
  — *Embed:* yes — <50 KB · *Size:* <50 KB · *Cadence:* nearly static; new actions every year or two
  — *Why:* Flags 'subject to an emissions consent decree — verify the approved emissions modification was performed'; directly affects resale and CARB-state registration.
- **Emissions testing (I/M) applicability by state/county** *(interesting)*
  Which states/counties run emissions inspection programs, with each program's model-year window and vehicle-type/GVWR exemptions — compiled once from EPA and ~30 state program pages, then computed against fields the decode already provides.
  — *Source:* EPA I/M program pages — https://www.epa.gov/state-and-local-transportation/vehicle-emissions-inspection-and-maintenance-im-information-state + state program rules
  — *Join:* model year + fuel type + GVWR (all decoded) + user-supplied state/county
  — *License:* EPA and state pages are government works, public domain; compilation is yours
  — *Embed:* yes — <50 KB · *Size:* <50 KB · *Cadence:* annual (programs change slowly)
  — *Why:* 'Does this specific vehicle need a smog check in <county>?' — very practical for buyers relocating vehicles.
- **Federal clean-vehicle tax credit eligibility (30D/25E/45W, historical)** *(interesting)*
  Which make/model/year EVs and PHEVs qualified for the federal new, used, and commercial clean-vehicle credits, with amounts, MSRP caps, and date windows, plus the IRS qualified-manufacturer registry. Credits terminated for vehicles acquired after 2025-09-30, so this is now a frozen table ideal for static embedding.
  — *Source:* IRS clean-vehicle credit pages + fueleconomy.gov Tax Center (tax2022/tax2023.shtml, taxused.shtml) + AFDC assembly-location list
  — *Join:* make+model+year (+MSRP cap and placed-in-service window returned as caveats); QM status at manufacturer/WMI level
  — *License:* US government work, public domain (IRS + DOE pages); HTML scrape, no bulk file
  — *Embed:* yes — <100 KB, mostly frozen · *Size:* <100 KB · *Cadence:* frozen since 2025-09-30; rare corrections
  — *Why:* 'Did this EV qualify and for how much' persists for used-EV shoppers and tax records; tiny cost to carry.
- **OpenEV charging & battery specs pack** *(interesting)*
  Community-maintained, schema-validated EV specs: charge port types (NACS/CCS1/CCS2/CHAdeMO/GB-T), AC onboard-charger kW, DC peak power and charging curves, battery gross/net kWh, chemistry, and preconditioning support, under a redistribution-friendly license.
  — *Source:* OpenEV Data — https://github.com/open-ev-data/open-ev-data-dataset (CDLA-Permissive-2.0); archived MIT fallback KilowattApp/open-ev-data
  — *Join:* make+model+year+variant (fuzzy text match to vPIC make/model/trim; needs a curated mapping table)
  — *License:* CDLA-Permissive-2.0 — explicitly redistribution-friendly; do NOT use commercial ev-database.org
  — *Embed:* yes — ~1-5 MB JSON releases · *Size:* ~1-5 MB · *Cadence:* rolling GitHub releases; pin a tag per monthly refresh
  — *Why:* Kills the #1 practical EV question vPIC can't answer: 'what plug does this car take and how fast can it charge?' — critical as the NACS transition splits model years mid-cycle.

### Theft, title & fraud

- **Cash for Clunkers scrapped-VIN registry** ⭐ *(wild)* — **verified 2/5 value, 5/5 feasibility**
  All ~677,000 trade-ins destroyed under the 2009 CARS program, at full-VIN level. Any of these VINs alive on the road today indicates VIN cloning — a static, public-domain fraud detector with zero refresh burden.
  — *Source:* US DOT/NHTSA CARS Final Paid Transaction Database — catalog.data.gov entry; original FTP dead, mirrored on Internet Archive
  — *Join:* full VIN — exact match against the scrapped set (verify VIN column on first ingest)
  — *License:* Federal agency transaction record — treat as public domain (data.gov lists 'unknown license'; note the metadata gap honestly)
  — *Embed:* yes — ~4-6 MB raw, ~1 MB as perfect-hash/Bloom set · *Size:* ~1-6 MB depending on encoding · *Cadence:* static (program ended 2009)
  — *Why:* 'This VIN was federally scrapped in 2009 — it should not exist' — offline VIN-fraud detection nobody else does.
- **NHTSA model-line theft rates (49 CFR 541)** ⭐ *(interesting)* — **verified 2/5 value, 4/5 feasibility**
  Statutorily mandated annual tables of thefts and theft rate per 1,000 vehicles produced, by manufacturer and model line, every year since MY1983/84 — the only official exposure-normalized per-model theft rate series in existence.
  — *Source:* NHTSA / Federal Register annual theft-data notices (e.g. federalregister.gov 2011-773, 2017-12883) + https://www.nhtsa.gov/road-safety/vehicle-theft-prevention/theft-rates
  — *Join:* make + model line + model year (one-time alias map from NHTSA line names to vPIC make/model)
  — *License:* US government work, public domain; Federal Register tables parseable as HTML/XML
  — *Embed:* yes — decades of data <500 KB · *Size:* <500 KB · *Cadence:* annual Federal Register notice (preliminary + final)
  — *Why:* 'This model gets stolen 6x more than average' informs insurance and parking decisions, and feeds clone-sniff heuristics; nobody surfaces this offline.
- **50-state total-loss & salvage/rebuilt title rules matrix** *(interesting)*
  Per-state total-loss threshold percentage or Total Loss Formula, mandatory brand rules, and rebuilt-title VIN-inspection requirements — ~50 rows of law-derived facts recompiled from statutes with citations.
  — *Source:* State statutes/insurance codes (public domain edicts); commercial compilations (carinsurance.com, wallethub) used only for cross-checking
  — *Join:* user-supplied titling state (+model year for older-vehicle exemptions); rules table, no VIN join
  — *License:* Law is public domain (government edicts doctrine); recompile facts yourself, never copy commercial tables verbatim
  — *Embed:* yes — <5 KB · *Size:* <5 KB · *Cadence:* annual review
  — *Why:* `ultravin title-rules TX` explains when a car becomes salvage-branded per state and where title-washing arbitrage exists.
- **FBI NIBRS motor-vehicle-theft context** *(obvious)*
  State/metro/year theft and recovery rates aggregated from incident-level NIBRS data. NIBRS carries no make/model, so this is contextual enrichment keyed on user-supplied state, not per-vehicle data.
  — *Source:* FBI Crime Data Explorer bulk downloads — https://cde.ucr.cjis.gov/
  — *Join:* none per-VIN — user-supplied registration state (or vPIC plant state for color)
  — *License:* US government work, public domain; explicit bulk CSVs
  — *Embed:* yes pre-aggregated — <50 KB (raw is multi-GB; aggregate in refresh pipeline) · *Size:* <50 KB aggregated · *Cadence:* annual bulk release
  — *Why:* Lets the CLI print 'MVT rate in <state>: X per 100k, recovery rate Y%' next to a decode.
- **Kia/Hyundai immobilizer-vulnerability flag** *(wild)*
  The ~40 model/year-range rows of 2011-2022 Hyundai/Kia vehicles built without engine immobilizers (the 'Kia Boys' epidemic) plus eligibility for the free anti-theft software campaign (claims accepted until 2027-03-31).
  — *Source:* NHTSA TSB MC-10247611 + hyundaiantitheft.com + IIHS analysis
  — *Join:* make+model+year (+vPIC keyless-ignition field to disambiguate turn-key vs push-button trims)
  — *License:* NHTSA TSB documents are public records; embedding a fact list, not manufacturer content
  — *Embed:* yes — <10 KB, effectively static now · *Size:* <10 KB · *Cadence:* static (campaign closes 2027)
  — *Why:* Arguably the single highest-impact theft warning a decoder can print in 2026, with a free-fix deadline attached.
- **NICB Hot Wheels most-stolen rankings** *(obvious)*
  Annual top-10 most-stolen vehicles nationally and per state, with make, model, most-stolen model year, and theft counts from NCIC reports. Tiny but famous.
  — *Source:* NICB annual Hot Wheels press release — https://www.nicb.org/news/news-releases/
  — *Join:* make+model+year (optionally per user-supplied state)
  — *License:* No open license — facts extracted from a private nonprofit's press release; flag as not openly licensed
  — *Embed:* yes by volume (<20 KB), with the licensing caveat · *Size:* <20 KB · *Cadence:* annual
  — *Why:* 'This exact model/year is the #1 stolen vehicle in your state' is a high-impact consumer output line.
- **NICB VINCheck theft/salvage lookup — link-out only** *(obvious)*
  NICB's free per-VIN lookup of unrecovered-theft (last ~5 years) and salvage/flood records: web form, 5 lookups/IP/day, no API, no bulk, ToS forbids replication. Cannot be embedded — at most ultravin prints the VINCheck URL as a suggested manual next step.
  — *Source:* https://www.nicb.org/vincheck (ToS at nicb.org/how-we-help/vincheck/terms-use-vincheck)
  — *Join:* full VIN (user-initiated, single lookups only)
  — *License:* Free consumer service, NOT open data; rate-limited, no redistribution
  — *Embed:* no — link-out in decode output only · *Size:* 0 (link-out) · *Cadence:* n/a
  — *Why:* 'Is it stolen or salvage?' is the top fraud check for private buyers; a rate-limit-respecting pointer closes a real UX gap honestly.
- **NMVTIS brand dictionary + state reporting map** *(obvious)*
  The ~66 standardized NMVTIS title-brand codes (salvage, flood, junk, lemon buyback...) with definitions, plus which states fully report to NMVTIS. Explicitly NOT per-VIN brands — those are locked behind paid providers with no bulk access.
  — *Source:* DOJ/BJA VehicleHistory.gov + AAMVA NMVTIS references + state guides (e.g. TxDMV NMVTIS guide)
  — *Join:* brand code (user-supplied from a title/report) + titling state; explain-and-contextualize layer, no VIN join
  — *License:* DOJ/BJA content public domain; per-VIN NMVTIS data is fee-based only — do not pretend otherwise
  — *Embed:* yes — <10 KB for the dictionary/state map · *Size:* <10 KB · *Cadence:* rarely changes; quarterly check
  — *Why:* `ultravin explain-brand FLD --state GA` decodes title-brand alphabet soup and warns when the titling state is a weak NMVTIS reporter (title-washing risk).
- **Odometer disclosure exemption computed flag (49 CFR 580)** *(interesting)*
  Pure rule, zero data: MY2010-and-older vehicles exempt under the old 10-year rule, MY2011+ exempt 20 years after model year, plus GVWR>16,000 lb exemptions — all computable from fields ultravin already decodes.
  — *Source:* eCFR 49 CFR 580.17 + NHTSA 2019 final rule (federalregister.gov 2019-25657)
  — *Join:* model year + GVWR from the existing vPIC decode; no external data
  — *License:* Public domain (federal regulation)
  — *Embed:* yes — ~20 lines of Rust, 0 bytes of data · *Size:* 0 KB (pure logic) · *Cadence:* regulatory; ~once a decade
  — *Why:* Flags 'odometer disclosure NOT federally required' — exactly the population where rollback fraud concentrates. High signal, literally free.
- **Parts-marking / high-theft line flag (Part 541 Appendix A)** *(interesting)*
  Official list of vehicle lines subject to federal parts-marking (VIN stamped on major parts) and lines exempted for shipping a factory immobilizer meeting Part 543 — two boolean flags per make/line/year.
  — *Source:* eCFR 49 CFR Part 541 appendices + annual Federal Register final-listing notices
  — *Join:* make + model line + model year
  — *License:* US government work, public domain
  — *Embed:* yes — <20 KB · *Size:* <20 KB · *Cadence:* annual
  — *Why:* Tells a buyer whether this vehicle's major parts should carry VIN stamps (chop-shop forensics) and whether the line certified with an immobilizer.
- **Salvage-auction presence (Copart/IAAI) — documented no-go** *(wild)*
  Whether a VIN appeared in a salvage auction run list — the raw signal behind 'this clean-title car was wrecked'. ToS prohibits scraping, no bulk export or open license exists; documented so the dead end prevents future wasted effort.
  — *Source:* copart.com / iaai.com run lists; third-party 'APIs' are ToS-violating scrapers or commercial feeds
  — *Join:* full VIN — but no lawful bulk source
  — *License:* COMMERCIAL/PROHIBITED — do not ship
  — *Embed:* no — no lawful bulk source; corpus would be GBs anyway · *Size:* n/a · *Cadence:* n/a
  — *Why:* Would be the highest-value title-fraud signal in existence, which is exactly why it is paywalled.
- **clone-sniff: VIN cloning / implausibility red flags** *(wild)*
  A derived heuristic layer, no external data: flags VINs whose serial number (vs serial-ceiling), plant/year consistency (vs plant-atlas), or pattern combination is statistically implausible despite a valid check digit — the signature of cloned/washed VINs. Outputs flags with reasons, never a verdict.
  — *Source:* Derived — combines ultravin's check-digit/pattern validity with serial-ceiling, plant-atlas, and theft-lines tables
  — *Join:* full VIN (it IS the input); sub-checks join on keys from the other datasets
  — *License:* Entirely derived from public-domain inputs; MIT, ultravin's own IP
  — *Embed:* yes — code plus tables already shipped by other ideas; near-zero marginal size · *Size:* ~0 (reuses other tables) · *Cadence:* inherits monthly refresh of inputs
  — *Why:* Fraud-screening is what people pay VIN services for; an honest offline 'this VIN smells wrong because X' is a headline `ultravin sniff` feature.

### International

- **Transport Canada vehicle recalls (VRDB)** ⭐ *(obvious)* — **verified 3/5 value, 5/5 feasibility**
  All Canadian motor vehicle, tire, and child-seat recalls with make, model, year range, system, defect, and units affected, published as a full monthly bulk CSV under an open license. Canadian campaigns often differ from or precede NHTSA ones.
  — *Source:* Transport Canada — https://open.canada.ca/data/en/dataset/1ec92326-47ef-4110-b7ca-959fab03f96d (vrdb_full_monthly.csv/xml) + API
  — *Join:* make+model+year (normalization to vPIC names needed)
  — *License:* Open Government Licence – Canada; redistributable
  — *Embed:* yes — ~3-5 MB compressed for the full history · *Size:* ~3-5 MB compressed · *Cadence:* monthly full CSV/XML dump
  — *Why:* Recall coverage for the huge cross-border used-car trade; the monthly bulk dump slots directly into existing refresh automation.
- **UK MOT reliability atlas (DVSA anonymised MOT results)** ⭐ *(interesting)* — **verified 3/5 value, 3/5 feasibility**
  Every GB roadworthiness test since 2005 (~40M/year) with make, model, age, odometer, outcome, and itemised failure reasons; ship a derived table of failure rates, top failure categories, and mileage percentiles per make+model+age band. Enables 'this 9-year-old model most likely fails on rear suspension and brake discs' wear forecasts plus expected-mileage curves for odometer-fraud sanity checks.
  — *Source:* DVSA — https://open.data.dvsa.gov.uk/mot-anonymised/index.html (mirrored on data.gov.uk)
  — *Join:* make+model+age (one-time normalization map of DVSA free-text strings to vPIC make/model; first-use year approximates model year)
  — *License:* Open Government Licence v3.0, Crown copyright — fully redistributable with attribution
  — *Embed:* yes — 2-5 MB aggregated (raw is ~2-3 GB/year, crunched in the refresh pipeline) · *Size:* 2-5 MB aggregated · *Cadence:* annual bulk release + newer extracts
  — *Why:* Real-world reliability is the #1 thing VIN decoders lack; failure-rate-by-component data is unique, defensible content ALLDATA charges for, built from free data.
- **ANCAP safety ratings** *(obvious)*
  Australasian NCAP star ratings and category scores (adult/child occupant, VRU, safety assist) with variant-level applicability and rating expiry years, for the AU/NZ fleet including models never tested by NHTSA/IIHS.
  — *Source:* https://www.ancap.com.au/ + government catalogue entry at catalogue.data.infrastructure.gov.au (returned 403 on verification)
  — *Join:* make+model+year (+build-date range and variant list)
  — *License:* Probably open (gov catalogue suggests CC BY) but unconfirmed; ANCAP site carries standard copyright — flag
  — *Embed:* yes — <500 KB (~1,000 ratings) · *Size:* <500 KB · *Cadence:* continuous; monthly scrape
  — *Why:* Safety stars for the AU/NZ fleet absent from any US-centric source.
- **Australian vehicle recalls** *(obvious)*
  All Australian road-vehicle recall notices with make, model, year ranges, defect — and notably, many notices attach exact affected-VIN lists or ranges. Likely CC BY 4.0 but no confirmed bulk dataset; requires scraping vehiclerecalls.gov.au.
  — *Source:* Australian Dept. of Infrastructure — https://www.vehiclerecalls.gov.au/recalls/browse-all-recalls (+ BITRE stats)
  — *Join:* make+model+year; VIN ranges/lists where published
  — *License:* Likely CC BY 4.0 (Australian gov default) but unverified for this site — flag
  — *Embed:* yes for metadata (~2 MB); partial with VIN lists · *Size:* ~2 MB metadata · *Cadence:* continuous; monthly scrape
  — *Why:* Third major English-language recall regime; VIN-level affected lists are gold, and the RHD/JDM-import market overlaps ultravin's international users.
- **EEA EU CO2 monitoring (WLTP specs per registered variant)** *(interesting)*
  Every new car registered in the EU since 2012 (37M+ rows, 8+ GB raw): make, commercial name, type/variant/version, type-approval number, WLTP CO2, mass, wheelbase, track widths, engine capacity/power, electricity consumption, and electric range. Aggregate to a variant-level spec table with registration counts in the refresh pipeline.
  — *Source:* European Environment Agency — https://co2cars.apps.eea.europa.eu/ + EEA datahub (Regulation (EU) 2019/631 monitoring data; Discodata SQL endpoint)
  — *Join:* make+model+year (fuzzy on commercial name/variant); type-approval number bridges to other TAN-bearing sources (RDW, Safety Gate); TVV is a stretch-goal VIN-descriptor bridge
  — *License:* EEA re-use policy — free commercial/non-commercial re-use with attribution (CC-BY-equivalent)
  — *Embed:* partial — raw is 8+ GB; aggregated variant table is ~5-15 MB compressed · *Size:* ~5-15 MB aggregated · *Cadence:* annual (provisional mid-year, final following year)
  — *Why:* Official WLTP CO2, mass, wheelbase, and EV range for EU-market vehicles vPIC will never cover, plus an EU popularity signal.
- **EU Safety Gate (RAPEX) motor-vehicle alerts** *(obvious)*
  EU-wide rapid alerts for dangerous products with motor vehicles as a top category: brand, model, type-approval number, often VIN/production ranges, defect and risk description, across 31 EEA countries.
  — *Source:* European Commission — https://ec.europa.eu/safety-gate-alerts/ (weekly-report XML API; also on data.europa.eu)
  — *Join:* make+model (+EU type-approval number; VIN/production ranges where listed)
  — *License:* EC reuse policy (Decision 2011/833/EU, CC-BY-equivalent); redistributable
  — *Embed:* yes — ~1-2 MB compressed for vehicle-category history · *Size:* 1-2 MB compressed · *Cadence:* weekly XML reports
  — *Why:* EU recall visibility that frequently flags defects before or independent of NHTSA campaigns.
- **Euro NCAP crash-test ratings** *(obvious)*
  Star ratings and per-category scores for essentially every volume model sold in Europe since 1997 — the deepest NCAP dataset in the world, but euroncap.com terms prohibit commercial reproduction, blocking clean MIT-style redistribution.
  — *Source:* https://www.euroncap.com/ (site JSON endpoints exist; third-party SOAP via regcheck.org.uk)
  — *Join:* make+model+test year (rating applies across a model generation)
  — *License:* NOT open — Euro NCAP copyright, commercial reproduction not authorised; ship only as facts with legal review, or an optional non-redistributed plugin
  — *Embed:* partial — tiny (~2,500 tests) but licensing blocks clean redistribution · *Size:* <1 MB · *Cadence:* result waves ~6-10x/year
  — *Why:* Huge user pull ('what did my 2019 Golf score?') for the many EU-built VINs ultravin decodes — which is exactly why the caveat must be flagged.
- **Euro emission-standard classifier (Euro 1-7)** *(interesting)*
  A tiny embedded rules table (not a download): EU type-approval and first-registration cutoff dates per vehicle category that determine which Euro standard a vehicle meets — the classification driving LEZ/ULEZ/Umweltzone access across European cities.
  — *Source:* Derived from EU legislation on EUR-Lex (Reg. (EC) 715/2007, (EU) 2017/1151, (EU) 2024/1257)
  — *Join:* model year + vehicle category/fuel type from the decode (reported as 'Euro 6 — typical for MY2016 M1 diesel' since registration date is unknown from VIN)
  — *License:* EU legal texts freely reusable (Decision 2011/833/EU); zero risk
  — *Embed:* yes — <10 KB, hand-curated once per decade · *Size:* <10 KB · *Cadence:* essentially static; new Euro standard every ~7-10 years
  — *Why:* Instantly answers 'is this car ULEZ/Crit'Air compliant?' — asked by millions of European used-car buyers, absent from vPIC.
- **French bonus écologique eco-score eligibility** *(interesting)*
  ADEME's official list of BEV versions that passed France's lifecycle 'score environnemental' (counting manufacturing carbon and transport) for the purchase incentive, keyed by type-variant-version — a de facto embedded-carbon dataset that notably excludes many China-built EVs.
  — *Source:* ADEME — https://data.ademe.fr/datasets/bonus-ecologique-score-environnemental (API/download)
  — *Join:* make+model (+TVV, corresponding to EU type-approval variant codes; date windows as caveats)
  — *License:* Licence Ouverte / Etalab (attribution, free redistribution) — confirmed
  — *Embed:* yes — <200 KB · *Size:* <200 KB · *Cadence:* irregular but frequent (last updated 2026-07-23)
  — *Why:* EU incentive eligibility plus a unique manufacturing-carbon angle: identical models diverge by build country.
- **German KBA recall database (Rückrufdatenbank)** *(interesting)*
  All recalls supervised by Germany's federal motor authority — brand, model/type, build period, defect, KBA reference — including administrative recalls (e.g. diesel emissions) that never hit RAPEX or NHTSA. No bulk download, no stated open license; scrape-only.
  — *Source:* Kraftfahrt-Bundesamt — kba.de Rückrufdatenbank search UI (revamped 2025)
  — *Join:* make+model + build-date range mapped against decoded model year; no VIN ranges published
  — *License:* Unclear — German federal data is often DL-DE/BY-2.0 but KBA does not say so; flag
  — *Embed:* partial — thousands of records (~1-2 MB) but scrape-only acquisition and unresolved licensing · *Size:* ~1-2 MB · *Cadence:* continuous; monthly scrape
  — *Why:* Authoritative coverage for the millions of German-built (W*) VINs ultravin decodes.
- **Gray-market import eligibility list** *(wild)*
  Every non-US-market vehicle NHTSA has ruled eligible for importation through a Registered Importer, with VSA/VCP eligibility numbers and make/model/year ranges — the authoritative answer to 'can this JDM/Euro car legally be here'.
  — *Source:* NHTSA Vehicle Importation — nonconforming-vehicles eligibility PDF + annual Federal Register republication
  — *Join:* make+model+year; pairs with WMI knowledge of non-US-built vehicles at decode time
  — *License:* US government work, public domain
  — *Embed:* yes — <200 KB (PDF/FR table parsing, stable format) · *Size:* <200 KB · *Cadence:* annual FR publication + rolling PDF updates
  — *Why:* Unique niche nobody serves offline: importers, JDM enthusiasts, and DMV clerks all fight this question. Perfectly on-brand.
- **Japan MLIT recall notifications (JDM/grey-import)** *(wild)*
  Japan's official recall registry (~5,000 notifications since 1993) with manufacturer, model, katashiki model code, chassis/frame-number ranges, and defect details — covering the JDM vehicles now flooding into the US/AU/NZ under 25-year import rules. Scrape-and-translate territory; the hard part is the katashiki-to-model mapping.
  — *Source:* MLIT — https://www.mlit.go.jp/en/jidosha/vehicle_recall.html (full DB in Japanese)
  — *Join:* weak — make+model+production period; JDM domestic vehicles use frame numbers, not 17-char VINs (flag frame-number-only imports explicitly)
  — *License:* Japanese Government Standard Terms of Use (CC-BY-4.0 compatible) — verify site-specific terms; translation/normalization effort flagged
  — *Embed:* yes — ~5 MB metadata · *Size:* ~5 MB · *Cadence:* continuous; monthly scrape
  — *Why:* A 1995 Skyline GT-R buyer in the US has no easy recall lookup today; even partial coverage of this unserved market is unique.
- **Latin NCAP + Global NCAP results** *(interesting)*
  Crash-test ratings for Latin-American-market vehicles (plus Global NCAP India/Africa campaigns), famous for exposing zero-star versions of models scoring 5 stars in Europe/US — same nameplate, stripped safety kit.
  — *Source:* https://www.latinncap.com/en/results (bulk results list offered) + globalncap.org
  — *Join:* make+model+year + plant country from the decoded WMI (distinguishes Mexican-built from EU-built versions)
  — *License:* Freely published, no explicit open license — flag as unclear; likely fine for factual star ratings
  — *Embed:* yes — <200 KB · *Size:* <200 KB · *Cadence:* a few waves/year
  — *Why:* For VINs with Mexican/Brazilian WMIs (3*/9*), surface the regionally-correct safety rating instead of the flattering US/EU one — genuinely differentiating.
- **NRCan Canada fuel consumption + BEV/PHEV ratings** *(obvious)*
  Canadian official ratings per model 1995-2026: L/100km city/hwy/combined, CO2 g/km, CO2/smog ratings, plus separate BEV/PHEV files with Canadian-cycle range, kWh/100km, and recharge time — metric units and Canada-only trims fueleconomy.gov misses.
  — *Source:* Natural Resources Canada — https://open.canada.ca/data/en/dataset/98f1a129-f628-4ce4-b24d-6f16bf24dd64 (CSV resources)
  — *Join:* make+model+year (+engine size/transmission for trim disambiguation)
  — *License:* Open Government Licence – Canada (attribution, commercial use permitted); verified
  — *Embed:* yes — ~2-5 MB total · *Size:* ~2-5 MB · *Cadence:* several releases per year
  — *Why:* Locally-correct official numbers for the Canadian-market VINs ultravin already decodes; format mirrors EPA data so it is cheap to add.
- **NZ vehicle fleet raw data (11-char VIN prefixes)** *(wild)*
  Monthly per-vehicle snapshot of the entire NZ fleet (5M+ rows) including the first 11 characters of each VIN — WMI+VDS+check digit+year+plant, a full VIN pattern — plus make, model, year, body, fuel, and odometer-derived usage, under CC-BY 4.0.
  — *Source:* NZ Transport Agency Waka Kotahi — https://nzta.govt.nz/resources/new-zealand-vehicle-fleet-raw-open-data-for-specialist-use
  — *Join:* VIN pattern (positions 1-11) — direct prefix match; among the strongest joins available
  — *License:* CC-BY 4.0 International — verified; redistributable with attribution
  — *Embed:* yes — ~10-30 MB pattern-aggregated as an optional extra package (raw ~1-2 GB) · *Size:* 10-30 MB aggregated · *Cadence:* monthly CSV release
  — *Why:* Millions of officially-registered VIN patterns for decoder validation, plus 'this pattern: 1,842 registered in NZ, median odometer 148,000 km' — including JDM imports that never touched the US.
- **Norway Autosys per-VIN technical lookup (online only)** *(wild)*
  Free government REST API returning near-complete technical data for any Norwegian-registered vehicle queryable by full VIN (50k calls/day/key) — but VINs are treated as personal data, so bulk harvesting and redistribution are off-limits. API-only by design.
  — *Source:* Statens vegvesen — https://autosys-kjoretoy-api.atlas.vegvesen.no/ (NLOD-licensed data)
  — *Join:* full VIN — the only source here with native full-VIN query
  — *License:* NLOD open license for data, but bulk redistribution of per-vehicle records restricted under Norwegian personal-data law
  — *Embed:* no — could power an explicit opt-in online enrichment flag; superb accuracy oracle for spot-validating the decoder · *Size:* n/a (API) · *Cadence:* live API
  — *Why:* Demonstrates the ceiling of VIN enrichment (exact per-vehicle specs) and serves as a government-grade validation oracle.
- **RDW Dutch national vehicle registry (CC0)** *(wild)*
  The entire Dutch vehicle register as CC0 open data (~17M vehicles, ~60 companion datasets): make, trade name, EU type-approval number, variant, mass, dimensions, emissions, inspection expiry, open-recall flags — plate-keyed (VINs withheld). Embed derived tables: type-approval specs, per-model dimension/mass medians, and use as decoder validation ground truth.
  — *Source:* RDW — https://opendata.rdw.nl/ (Gekentekende_voertuigen m9d7-ebf2, Socrata, refreshed daily)
  — *Join:* make+model+year for aggregates; EU type-approval number as bridge key to EEA CO2 and Safety Gate; no direct VIN join
  — *License:* CC0 / public domain — the gold standard; redistribute freely
  — *Embed:* partial — raw ~10 GB; derived tables 3-10 MB · *Size:* 3-10 MB derived · *Cadence:* daily on Socrata; monthly snapshot
  — *Why:* A free ground-truth mine of variant-level dimensions/masses vPIC lacks for EU vehicles, plus decode validation for EU-built models.
- **UK VCA car fuel & CO2 emissions data (WLTP)** *(obvious)*
  UK-market new-car data per make/model/derivative: WLTP CO2 g/km (the figure that sets UK VED tax bands), fuel consumption, BEV/PHEV electric range, noise, and emissions standard.
  — *Source:* UK Vehicle Certification Agency — https://carfueldata.vehicle-certification-agency.gov.uk/downloads/default.aspx (CSV)
  — *Join:* make+model (+derivative text and year-of-introduction) — coarser join; surface candidate matches
  — *License:* UK Open Government Licence v3.0; redistributable
  — *Embed:* yes — ~5 MB compressed · *Size:* ~5 MB · *Cadence:* roughly monthly
  — *Why:* WLTP numbers (materially different from EPA) plus the CO2 figure that determines UK road tax.
- **UK fleet survival curves (DfT VEH0120)** *(interesting)*
  Quarterly counts of every make/model/fuel licensed in the UK since 1994 (the dataset behind howmanyleft.co.uk); derive survival curves — what fraction of a cohort is still on the road N years later — and rarity indicators.
  — *Source:* UK DfT — https://www.gov.uk/government/statistical-data-sets/vehicle-licensing-statistics-data-files (table VEH0120)
  — *Join:* make+model (+model year via cohort tracking); GenModel strings need normalization to vPIC names
  — *License:* Open Government Licence v3.0; redistributable
  — *Embed:* yes — 2-5 MB derived (raw ~38 MB CSV) · *Size:* 2-5 MB derived · *Cadence:* quarterly
  — *Why:* '15-year survival rate 22% vs 61% class average' is a durability signal no spec sheet provides, complementing the MOT failure data.

### Valuation & market

- **Bring a Trailer / Cars & Bids collector-market price pulse** *(interesting)*
  Median/percentile realized sale prices per make+model+generation from enthusiast auction results — the de facto collector price index; BaT listings even publish full VINs, enabling exact-VIN provenance hits. ToS restricts scraping; do not ship without a data agreement.
  — *Source:* https://bringatrailer.com/auctions/results/ + https://carsandbids.com/past-auctions/ (no official API or bulk export)
  — *Join:* make+model+year; many BaT listings carry full VINs ('this exact car sold on BaT in 2023')
  — *License:* UNCLEAR/COMMERCIAL — prices-as-facts arguably uncopyrightable but redistribution risk is real; community-contributed price observations are the clean alternative
  — *Embed:* partial — aggregates ~2-5 MB, but licensing is the blocker · *Size:* 2-5 MB aggregated · *Cadence:* auctions daily; monthly if ever licensed
  — *Why:* Even a coarse 'collector-market median for this generation: $34k (n=212)' is unprecedented in an offline tool.
- **GSA fleet resale prices (public-domain depreciation curves)** *(wild)*
  Federal surplus vehicle sales — GSA Fleet alone sells 30k+ maintained vehicles/year with VIN, mileage, condition, and realized prices. Accumulating monthly snapshots yields the only public-domain wholesale price/depreciation curves by make+model+year+mileage band; caveat: ultravin must build its own history since GSA publishes no bulk archive.
  — *Source:* GSA Auctions API — https://gsa.github.io/auctions_api/ (api.data.gov key) + https://marketplace.gsafleet.gov/sales/browse-vehicles
  — *Join:* full VIN on sale records; aggregated curves join on make+model+year+mileage band
  — *License:* US government work, public domain
  — *Embed:* yes — <1 MB embedded curves; raw archive stays in the refresh pipeline · *Size:* <1 MB embedded · *Cadence:* continuous auctions; monthly snapshot
  — *Why:* The only free, redistributable answer to 'what is this car actually worth' in a space that is otherwise 100% proprietary (KBB/Manheim/Black Book).
- **HLDI insurance-loss & theft-claim ratings by series** *(interesting)* — **verified 4/5 value, 1/5 feasibility**
  Relative insurance-loss results (percent above/below all-vehicle average) for hundreds of series under six coverages, plus the whole-vehicle-theft report's exposure-normalized claim frequency and severity (Camaro ZL1 at 39x average, Hyundai/Kia surge). Published as free PDFs but copyrighted with no open license.
  — *Source:* IIHS-HLDI — https://www.iihs.org/research-areas/auto-insurance/insurance-losses-by-make-and-model + annual collision/theft report PDFs (e.g. hldi_theft_WT-24.pdf)
  — *Join:* make+series+body style+model-year band (curated map from HLDI series names to vPIC, same shape as the NCSA mapping vPIC ships)
  — *License:* UNCLEAR/COPYRIGHTED — facts-vs-compilation gray area; consider asking HLDI for permission or ship as separately-downloaded extra
  — *Embed:* partial — data is tiny (<500 KB) but licensing unresolved and acquisition is PDF-scraping · *Size:* <500 KB · *Cadence:* annual reports + ad hoc bulletins
  — *Why:* The best real-world 'expensive to insure / gets stolen / crashes worst' signal — the commercial counterpart to NHTSA's lab data.
- **J.D. Power (NADA) VIN-precise valuation adapter** *(obvious)*
  Successor to NADA guides: trade-in, loan, and retail values with VIN Precision+ as-built option pricing via commercial REST API. Contract-only, no redistribution — ship only a thin optional adapter with user-supplied credentials.
  — *Source:* J.D. Power Valuation Services — https://b2b.nada.com/get-values/nada-web-service-used-car-commercial-truck
  — *Join:* full VIN
  — *License:* Fully commercial, contract-only
  — *Embed:* no — adapter code only · *Size:* 0 (adapter) · *Cadence:* vendor monthly; live API
  — *Why:* Lenders and dealers who adopt ultravin invariably need book values next; `ultravin value --provider jdpower` keeps them in the tool.
- **Kaggle used-car auction prices (static depreciation baseline)** *(wild)*
  ~500k US wholesale auction transactions (circa 2014-2015) with VIN, sale price, MMR value, mileage, condition — frozen but large enough to fit baseline depreciation curves. Provenance is murky (appears scraped); treat as prototype fuel for the GSA approach, not a shippable dataset.
  — *Source:* Kaggle — tunguz 'Used Car Auction Prices' + ananaymital 'US Used cars dataset'
  — *Join:* full VIN on raw rows; fitted curves on make+model+year+age+mileage
  — *License:* UNCLEAR — no clean open license; embedding raw rows not defensible; fitted aggregates lower-risk but still flagged
  — *Embed:* partial — aggregates only, clearly labeled, or not at all · *Size:* <1 MB as fitted curves · *Cadence:* static (never updates)
  — *Why:* Instant offline 'typical depreciation for this model' to prototype valuation before the public-domain GSA archive accumulates.
- **MSRP & trim-level spec CSVs (CarAPI / CarQuery)** *(obvious)*
  Year/make/model/trim tables with original MSRP and consumer-grade specs, downloadable as CSV under a cheap subscription — but standard plans do not grant redistribution inside an MIT wheel; needs explicit written permission. Fills the one consumer field vPIC pointedly lacks: what the car cost new.
  — *Source:* CarAPI.app — https://carapi.app/features/vehicle-csv-download/ ($199-299/yr); legacy free CarQuery data is stale with dubious provenance
  — *Join:* make+model+year+trim (fuzzy map to vPIC Series/Trim)
  — *License:* Freemium-commercial; redistribution not granted by standard plans — flag
  — *Embed:* partial — embed-ready technically, blocked on redistribution rights · *Size:* a few MB (MSRP extract) · *Cadence:* annual model-year additions
  — *Why:* Original MSRP by trim is the most-asked pricing datum for total-loss, classic valuation, and 'what did it sticker for'.
- **MarketCheck live market price & days-on-market adapter** *(obvious)*
  5B+ US/CA dealer listings since 2015: current asking prices, price-drop history, days-on-market, per-VIN listing history, and predicted price. Commercial pay-per-use API with a free dev tier — optional adapter only.
  — *Source:* MarketCheck Cars API — https://www.marketcheck.com/apis/cars/
  — *Join:* full VIN (listing history) or make+model+year+trim (market stats)
  — *License:* Commercial; no bulk redistribution
  — *Embed:* no — live data by nature; adapter only · *Size:* 0 (adapter) · *Cadence:* daily vendor-side; live API
  — *Why:* 'What are these actually listed for right now' and 'has this exact VIN been for sale before' for dealers and arbitrage tooling.
- **Verisk VINMASTER / ISO insurance rating symbols — honest no-go** *(wild)*
  Vehicle Series Rating Symbols used by nearly every US personal-auto insurer, delivered only under enterprise contracts — no public API, no bulk, no redistribution. Listed to document the gap; realistic shape is a partnership or simply documenting that ultravin's pattern-level decode aligns with insurer symbol workflows.
  — *Source:* Verisk — https://www.verisk.com/products/vinmaster/
  — *Join:* VIN pattern (positions 1-8+10) — exactly the granularity ultravin decodes at
  — *License:* Strictly proprietary enterprise licensing
  — *Embed:* no — contractual only · *Size:* 0 · *Cadence:* vendor-managed
  — *Why:* Insurtech is a natural customer segment; acknowledging the VINMASTER-shaped hole positions ultravin inside insurer rating pipelines.

### Creative / derived

- **FARS fatal-crash VIN analytics (stats + bloom filter)** *(wild)*
  Two products derived from 30 years of FARS vehicle records (which carry VINs, plus NHTSA-published VPICDECODE companion files since 2019): (a) fatal-involvement rates, pattern-frequency priors, and cohort survival proxies per make/model/year, and (b) a ~2 MB bloom filter of ~1.5M crash-involved VINs so a decode can whisper 'this exact VIN appears in a fatal-crash record'.
  — *Source:* NHTSA FARS annual CSVs — https://static.nhtsa.gov/nhtsa/downloads/FARS/ (e.g. FARS2023NationalCSV.zip, 34 MB)
  — *Join:* VIN / VIN pattern (prefix+serial match; truncated in some years) for the filter; make/model/year via NCSA codes vPIC already maps for stats
  — *License:* US government work, public domain
  — *Embed:* yes — stats <1 MB; bloom filter ~2 MB at 10 bits/element (raw ingested only at refresh time) · *Size:* ~3-8 MB derived · *Cadence:* annual FARS release; monthly refresh polls for new years
  — *Why:* A salvage-history-adjacent signal for free plus honest per-model fatality-rate stats — no other offline tool has either; catnip for data journalists and fleet analysts.
- **NY DMV registration popularity (make+year rarity index)** *(interesting)*
  ~12M active NY registrations with make, model year, body type, fuel, county — no VIN or full model name, so aggregate to registration counts by make+year(+body+county) as a commonality/rarity score covering ALL vehicle types, not just EVs.
  — *Source:* NY DMV via Open Data NY — https://data.ny.gov/Transportation/Vehicle-Snowmobile-and-Boat-Registrations/w4pv-hbkt
  — *Join:* make + model year (+body class) — coarser than the WA join
  — *License:* Open Data NY terms — broadly reusable state open-data program; verify terms link at ingest
  — *Embed:* yes aggregated — <500 KB (make+year) to ~5 MB (with county); raw is ~2 GB · *Size:* <500 KB to ~5 MB · *Cadence:* portal updates roughly annually
  — *Why:* 'How common is this vehicle?' informs theft-targeting risk, parts availability, and collector rarity.
- **chassis-code-codex: internal codes + enthusiast nicknames** *(obvious)*
  Maps make+model+year to internal chassis/platform codes (BMW E/F/G, Mercedes W-numbers, Toyota JZA80, VW Mk) and community nicknames ('Hakosuka', 'Foxbody') from Wikidata generation labels and aliases — ships as one extra column on the genealogy table.
  — *Source:* Wikidata generation items (named by chassis code) + aliases; Wikipedia model-code articles as cross-check
  — *Join:* make+model+model year → generation code (same join machinery as model-genealogy)
  — *License:* Wikidata CC0
  — *Embed:* yes — <500 KB marginal · *Size:* <500 KB marginal · *Cadence:* weekly dumps; monthly refresh
  — *Why:* Enthusiast search speaks chassis codes, not vPIC series names; 'decode → E46' is instant credibility with the tuner/collector crowd.
- **defunct-brands: dead-marque registry** *(obvious)*
  For every dead make: years active, fate (bankruptcy/merger/absorbed), and successor brand — decode a Saab, Pontiac, or Fisker and get 'brand discontinued 2010, absorbed into GM wind-down'.
  — *Source:* Wikipedia defunct-manufacturer lists cross-referenced to Wikidata items (dissolution date, successor) via CC0 dumps
  — *Join:* make (and WMI for precision) — vPIC decodes dead brands fine, it just says nothing about their death
  — *License:* Wikidata CC0 for structured facts (no prose copied)
  — *Embed:* yes — <500 KB · *Size:* <500 KB · *Cadence:* brands die slowly; monthly refresh free
  — *Why:* Orphan-vehicle owners (parts, insurance, registration headaches) and collectors get context every decoder omits.
- **fleet-package-codes: police/taxi/fleet VIN codes** *(interesting)*
  Curated table mapping VIN-encoded body/series codes to fleet roles — Crown Vic P71 police / P72 taxi, Charger Pursuit, Caprice/Tahoe PPV — honestly recording where the code is NOT VIN-determinable (e.g. Chevy RPO 9C1).
  — *Source:* Manufacturer body-code documentation as recorded on Wikipedia (e.g. Ford CVPI VIN positions 5-7) and equivalent pursuit-vehicle docs
  — *Join:* VIN positions 5-7 (or vPIC Series/Trim) + make + year range
  — *License:* Facts about VIN encoding are not copyrightable; compiled table is ultravin's own, MIT
  — *Embed:* yes — ~10 KB · *Size:* ~10 KB · *Cadence:* nearly static; occasional hand-curation
  — *Why:* 'Is this used Crown Vic an ex-cop car' is a real purchase question; also feeds clone-sniff (distinct theft/resale patterns).
- **model-genealogy: predecessor/successor timelines** *(interesting)*
  A family tree per model from Wikidata P155/P156 links and generation items with production year ranges: decode a 2004 BMW 330i and get 'generation E46 (1997-2006), preceded by E36, succeeded by E90'.
  — *Source:* Wikidata bulk JSON dumps — automobile-model and generation items via P155 'follows' / P156 'followed by'
  — *Join:* make+model+model year → generation item selected by year range (label-matching table built at refresh time)
  — *License:* Wikidata CC0 — fully redistributable
  — *Embed:* yes — 2-5 MB after CI-side extraction · *Size:* 2-5 MB compressed · *Cadence:* weekly dumps; monthly refresh
  — *Why:* No decoder ships lineage; enthusiasts, listings sites, and insurance modelers all want generation identity, which vPIC lacks.
- **plant-atlas: assembly-plant geocoding + history** *(interesting)*
  Lat/long, opened/closed dates, owner history, and status for every automobile assembly plant, turning vPIC's bare plant-city string into 'built at Fort Wayne Assembly (41.02N, -85.30W), GM, opened 1986, still active'.
  — *Source:* Wikidata SPARQL (automobile-factory items with P625 coords + operational dates); Wikipedia plant articles as cross-check
  — *Join:* vPIC plant city+state+country+manufacturer fuzzy-matched at BUILD time into a static table; runtime join is exact
  — *License:* Wikidata CC0; dates are facts — clean for MIT redistribution
  — *Embed:* yes — <1 MB for a few thousand plants · *Size:* <1 MB · *Cadence:* weekly Wikidata dumps; monthly refresh
  — *Why:* 'Where was my car born, exactly' is the most-asked fun VIN question; also enables plant-closure trivia and map UIs, and feeds clone-sniff plant/year checks.
- **production-rarity: official production figures** *(obvious)*
  Total units produced per model/generation where published (Wikidata P1092 'total produced'), emitted as a rarity tier: mass-market / uncommon / rare / unicorn. Coverage is strongest exactly where rarity matters — classics and enthusiast cars — and degrades gracefully.
  — *Source:* Wikidata P1092 harvested from CC0 bulk dumps
  — *Join:* make+model+model year → generation item (year-range match)
  — *License:* Wikidata CC0
  — *Embed:* yes — <1 MB (sparse) · *Size:* <1 MB · *Cadence:* weekly dumps; monthly refresh
  — *Why:* 'How rare is my car' is a killer feature no free decoder has.
- **recall-population-proxy: fleet-size floors from recall counts** *(interesting)*
  Derived dataset: for each make+model+year, the max 'potentially affected' count across recall campaigns is a hard lower bound on US units sold — a production/fleet-size estimate NHTSA effectively publishes for free, filling the gaps Wikidata leaves for ordinary modern cars.
  — *Source:* NHTSA ODI recalls flat file (unit counts per campaign) — data.transportation.gov izrd-p2z4 / FLAT_RCL.zip
  — *Join:* make+model+model year
  — *License:* US government work, public domain
  — *Embed:* yes — ~1-2 MB aggregated · *Size:* 1-2 MB · *Cadence:* source updated continuously; monthly refresh
  — *Why:* Combined with production-rarity it gives near-complete rarity coverage.
- **screen-cars: movie & video-game appearances** *(interesting)*
  Per make+model+generation: appearance counts and top famous credits ('1968 Mustang GT390 — Bullitt') from IMCDb/IGCD — but neither offers an API, bulk dump, or open license, so shipping requires maintainer permission; a Wikipedia-sourced famous-cars subset (<1 MB, CC BY-SA facts) is the shippable fallback.
  — *Source:* IMCDb.org + IGCD.net (community databases, active, no API); Wikipedia 'in popular culture' as fallback
  — *Join:* make + model + model-year range
  — *License:* UNCLEAR/NOT REDISTRIBUTABLE as-is — do not ship without permission
  — *Embed:* partial — ~10-20 MB if licensed; <1 MB Wikipedia subset shippable today · *Size:* 10-20 MB full / <1 MB subset · *Cadence:* quarterly if licensed
  — *Why:* Pure delight: 'your car was in 47 movies' makes ultravin go viral in a way displacement fields never will.
- **serial-ceiling: production floors from observed sequential numbers** *(wild)*
  For each WMI+model-year, the maximum observed sequential production number (VIN positions 12-17) across millions of real VINs from FARS, ODI complaints, and WA registrations — a lower bound on units built, and your own VIN's serial becomes a percentile: 'roughly #8,450 of at least 31,000 built'.
  — *Source:* Derived from FARS VINs, NHTSA ODI FLAT_CMPL.zip VIN column, and WA EV data (all public domain or open)
  — *Join:* full VIN — WMI+model year selects the row; the input VIN's serial computes the percentile locally
  — *License:* All inputs public domain/open; derived table is ultravin's own work, MIT
  — *Embed:* yes — <1 MB (one max-serial integer per WMI+year) · *Size:* <1 MB · *Cadence:* recomputed monthly as corpora grow
  — *Why:* A genuinely novel 'birth certificate' stat requiring exactly the VIN-corpus pipeline ultravin's refresh automation already is; also feeds clone-sniff.
- **wiki-halo: popularity score + article link + free photo** *(wild)*
  Per model/generation: a 12-month Wikipedia pageview total as an 'enthusiast heat' score, the article URL, and a Commons image URL with license tag — a cultural-relevance ranking where a Supra outranks a Corolla despite 1/100th the production.
  — *Source:* Wikimedia pageview-complete dumps joined to model articles via Wikidata sitelinks; representative image from P18 (Commons)
  — *Join:* make+model+year → Wikidata item (same genealogy join) → sitelink + P18
  — *License:* Pageview stats freely reusable; Wikidata CC0; ship image URL + license tag, not the bytes
  — *Embed:* yes for scores/URLs (~1-2 MB); image bytes stay online by design · *Size:* 1-2 MB · *Cadence:* dumps daily/monthly; refresh monthly
  — *Why:* Powers 'interestingness' sorting in any app built on ultravin.

### Commercial vehicle & trucking (FMCSA)

- **truck-oos-atlas: per-model out-of-service & violation-rate scores** *(interesting)*
  Every US roadside inspection of trucks, tractors, buses, and trailers carries the unit VIN plus vehicle violation and out-of-service totals. Decode the 13.7M VINs with ultravin itself (dogfooding), then aggregate to per-make/model/year vehicle OOS rate, violations-per-inspection, and clean-inspection share — the used-truck buyer's equivalent of the UK MOT reliability atlas, for Class 3-8 trucks, motorcoaches, and trailers.
  — *Source:* FMCSA MCMIS via DOT Open Data (Socrata): Inspections Per Unit https://data.transportation.gov/Trucking-and-Motorcoaches/Inspections-Per-Unit/wt8s-2hbx (13.68M unit rows, VIN in insp_unit_vehicle_id_number — verified by sampling) joined to Vehicle Inspection File https://data.transportation.gov/Trucking-and-Motorcoaches/Vehicle-Inspection-File/fx4q-ay7w (8.31M inspections, 2023-07..2026-07, per-inspection vehicle_viol_total / vehicle_oos_total). Bulk CSV export via /api/views/<id>/rows.csv?accessType=DOWNLOAD
  — *Join:* Aggregates keyed by make+model+year (built by batch-decoding the file's full VINs); joins to any decoded VIN at lookup time
  — *License:* US government work (FMCSA/DOT), public domain under 17 USC 105; published free through the FMCSA data dissemination / Open Data program. catalog.data.gov formally lists 'license unspecified'; only driver PII is withheld, vehicle records are fully public
  — *Embed:* yes — aggregated table is tiny (~50k model-year rows); raw source CSVs are ~1-2 GB but processed at refresh time, not shipped · *Size:* ~1-2 MB compressed in-wheel · *Cadence:* Socrata datasets refresh daily (rowsUpdatedAt verified as today); rolling ~3-year window fits the existing monthly refresh automation
  — *Why:* Answers the question every used-truck and fleet buyer has and no free tool answers: which tractor/trailer models actually get put out of service at roadside. Directly monetizable insight vPIC decodes the population for but never scores
- **violation-fingerprint: what breaks on this model (49 CFR component profile)** *(wild)*
  Each violation row cites the exact 49 CFR section (393.47 brakes out of adjustment, 393.9 inoperable lamps, 393.75 tires, 396.3 general maintenance...) and whether it triggered OOS, and links to the unit's VIN. Aggregating per model-year yields a component failure fingerprint: brakes vs lighting vs tires vs steering vs suspension vs coupling shares, normalized per inspection.
  — *Source:* FMCSA MCMIS via DOT Open Data: Vehicle Inspections and Violations https://data.transportation.gov/Trucking-and-Motorcoaches/Vehicle-Inspections-and-Violations/876r-jsdb (13.41M violation rows with viol_code, part_no/part_no_section, out_of_service_indicator, insp_unit_id) joined to unit VINs in https://data.transportation.gov/Trucking-and-Motorcoaches/Inspections-Per-Unit/wt8s-2hbx via inspection_id + unit
  — *Join:* Aggregates keyed by make+model+year (from ultravin-decoded VINs); joined to decoded VIN at lookup
  — *License:* US government work, public domain (17 USC 105); free bulk download, no restrictions on vehicle-level data
  — *Embed:* yes — model-year x ~15 violation-category matrix compresses to a few MB · *Size:* ~3-5 MB compressed · *Cadence:* daily at source; monthly snapshot in refresh automation
  — *Why:* Turns a decode into a pre-purchase checklist: 'this tractor model's OOS hits are disproportionately brake adjustment; that one throws lighting violations.' Nothing comparable exists outside paid fleet-maintenance benchmarks
- **cmv-crash-ledger: DOT-recordable crash history per VIN and per model** *(interesting)*
  Every DOT-recordable crash (tow-away, injury, or fatality) involving a commercial motor vehicle, with the truck/bus VIN. Two products: (a) per-VIN flag — 'this specific used truck has N recordable crashes, worst outcome tow-away/injury/fatal' via bloom filter + optional detail pack; (b) per-model crash-involvement rates normalized by inspection exposure from the truck-oos-atlas denominator. Complements the already-collected FARS idea, which covers fatal crashes only — this adds the ~97% of recordable CMV crashes that are non-fatal.
  — *Source:* FMCSA MCMIS Crash File via DOT Open Data: https://data.transportation.gov/Trucking-and-Motorcoaches/Crash-File/aayw-vxb3 — 4.97M state-reported CMV crash records from 1982 to present (verified), with vehicle_identification_number (98.6% fill on post-2023 rows, verified by SODA query), fatalities, injuries, tow_away, hazmat_released, gvw_rating, vehicle_configuration
  — *Join:* Full VIN (per-VIN ledger); make+model+year for the rate aggregates
  — *License:* US government work, public domain (17 USC 105); driver info withheld, vehicle and carrier fields public
  — *Embed:* yes — bloom filter of crash-involved VINs (~5 MB at 1% FPR) in the main wheel; full per-VIN detail as optional extra package · *Size:* ~5 MB bloom in-wheel; ~60-120 MB optional full ledger · *Cadence:* updated daily at source (max report_date was 2 days ago); monthly snapshot
  — *Why:* A free offline 'has this truck been in a reportable wreck' check — the single highest-anxiety question in used heavy-truck buying, normally answered only by paid RigDig/Carfax-style reports
- **vin-fleet-provenance: who ran this truck (inspection sighting history)** *(wild)*
  Every roadside inspection pins a VIN to a USDOT number, date, state, and outcome. Inverting this gives a per-VIN provenance record: which carriers operated the unit, when and where it was seen, how many inspections it took, OOS hits, and last clean CVSA decal. Condensed form: VIN → (last USDOT, carrier name, sighting count, OOS count, last-seen date/state).
  — *Source:* Same MCMIS trio on DOT Open Data: Inspections Per Unit (wt8s-2hbx, VIN + insp_unit_decal CVSA-decal flag) x Vehicle Inspection File (fx4q-ay7w: dot_number, insp_date, report_state, carrier name, OOS totals). 13.7M sightings over rolling ~3 years
  — *Join:* Full VIN
  — *License:* US government work, public domain (17 USC 105). Caveat: Socrata window is ~3 years rolling; deeper history requires ordering historical MCMIS extracts from FMCSA's data dissemination program (fee-based), so ship the free window
  — *Embed:* partial — condensed VIN→last-carrier map ~50-80 MB compressed (optional extra package, like a 'ultravin-provenance' wheel); too big for the core wheel · *Size:* ~50-80 MB compressed (condensed); ~200-300 MB full sightings · *Cadence:* daily at source; monthly package rebuild
  — *Why:* Used-truck provenance for free: 'this 2019 Cascadia ran for a Texas mega-fleet, took 14 inspections, 3 vehicle OOS, last seen March 2026.' Also yields the VIN→USDOT bridge that powers the carrier-safety sidecar
- **carrier-safety-sidecar: safety profile of the fleet that operated the VIN** *(interesting)*
  A trimmed snapshot of carrier safety context — carrier OOS rates vs national average, authority revocations, insurance lapses, federal out-of-service orders — keyed by USDOT number and bridged to VINs through the vin-fleet-provenance VIN→USDOT map. Tells a buyer whether the truck's operator was a well-run fleet or a chameleon carrier running on lapsed insurance.
  — *Source:* FMCSA carrier-level open data: Company Census File https://data.transportation.gov/Trucking-and-Motorcoaches/Company-Census-File/az4n-8mr2 (fleet size, cargo, operation type), OUT OF SERVICE ORDERS https://data.transportation.gov/Trucking-and-Motorcoaches/OUT-OF-SERVICE-ORDERS/p2mt-9ige, authority/insurance history (e.g. https://data.transportation.gov/dataset/Insur-All-With-History/ypjt-5ydn, AuthHist 9mw4-x3tu), and SMS raw safety data https://ai.fmcsa.dot.gov/SMS — all updated daily-to-monthly
  — *Join:* Derived: VIN → USDOT (via inspection sightings) → carrier profile; no direct VIN key, honestly a two-hop join
  — *License:* US government work, public domain. Caveat: since the FAST Act (2015) FMCSA does not publicly display property-carrier SMS BASIC percentiles; ship raw OOS rates, authority, insurance, and OOS-order facts, not reconstructed percentile scores
  — *Embed:* yes if trimmed to the ~300k carriers appearing in inspection data (~5-10 MB); full 2M+ census ~40-60 MB · *Size:* ~5-10 MB compressed (trimmed) · *Cadence:* census/insurance/OOS-order files refresh daily on Socrata; monthly snapshot
  — *Why:* Vehicle condition correlates with operator quality — a truck from a revoked-authority, 3x-OOS-rate carrier is a different purchase than an identical unit from a blue-chip fleet. No free tool connects VIN to operator safety record
- **active-fleet-census: observed US commercial-fleet population by model** *(interesting)*
  Counting distinct inspected VINs per make+model+year (and per state) yields a de facto census of the active on-road Class 3-8 fleet — how many 2018 Peterbilt 579s are still working US highways, where dry-van trailer makes concentrate, median fleet age per segment. Also the exposure denominator that makes the OOS and crash rates statistically honest.
  — *Source:* Byproduct of the same MCMIS inspection unit file (wt8s-2hbx): ~8M distinct VINs of trucks/buses/trailers actually inspected on US roads over 3 years, with state from the joined inspection file (fx4q-ay7w)
  — *Join:* make+model+year (aggregates from decoded VINs); state dimension optional
  — *License:* US government work, public domain (17 USC 105)
  — *Embed:* yes — aggregate counts, ~1 MB · *Size:* ~1 MB compressed · *Cadence:* daily at source; monthly rebuild
  — *Why:* Commercial-vehicle rarity/popularity index (the existing NY-DMV and production-rarity ideas cover consumer vehicles; nothing covers working trucks) plus survivorship signal: units still being inspected at 15 years old are demonstrably still alive
- **smartway-ghg-flag: EPA SmartWay designated tractor/trailer flag** *(obvious)*
  The small set of sleeper/day-cab tractor models (and trailer manufacturers/configs) carrying EPA SmartWay designation — aerodynamic + low-rolling-resistance certified. Matters legally: CARB's Tractor-Trailer GHG regulation requires SmartWay-certified equipment for 53-ft box trailers and 2011+ sleeper tractors operating in California.
  — *Source:* EPA SmartWay designated tractors and trailers https://www.epa.gov/verified-diesel-tech/smartway-designated-tractors-and-trailers plus CARB's mirror list of US EPA SmartWay designated tractor models https://ww2.arb.ca.gov/sites/default/files/classic/cc/hdghg/tractor.php (page exists but blocks bot fetches; CARB Tractor-Trailer GHG regulation pages list the same models)
  — *Join:* make+model+year for tractors; manufacturer-level only for trailers (flagged honestly — trailer designation is not model-year precise)
  — *License:* US government (EPA/CARB) content, public domain — but no clean bulk file exists; requires scraping HTML/PDF lists, and EPA reorganized these pages, so source stability is the real risk
  — *Embed:* yes — trivially small; the honest caveat is acquisition (scrape), not size · *Size:* <100 KB · *Cadence:* designation lists change roughly annually with model years; monthly check is more than sufficient
  — *Why:* One-bit answer to 'can this tractor/trailer legally run California drayage/linehaul under the GHG rule, and is it the fuel-efficient spec' — useful in used Class 8 listings where SmartWay spec commands a price premium

### ADAS & automated driving

- **sgo-adas-crash-pack: Level-2 driver-assist crash reports by VIN pattern** *(interesting)*
  Every crash a manufacturer was required to report where a Level-2 ADAS (Autopilot, BlueCruise, Super Cruise, SCC+LFA, ...) was engaged within 30 seconds of impact. ~120 columns per report: make/model/year, the named automation feature version, engagement status, crash partner, injury severity, airbag deployment, roadway/weather, redacted narrative. Verified 2,270 reports in the current rolling-year file alone; 100% of rows carry a VIN field.
  — *Source:* NHTSA Standing General Order 2021-01, https://www.nhtsa.gov/laws-regulations/standing-general-order-crash-reporting — bulk CSV at https://static.nhtsa.gov/odi/ffdd/sgo-2021-01/SGO-2021-01_Incident_Reports_ADAS.csv (rolling year) plus Archive-2021-2025 folder at https://static.nhtsa.gov/odi/ffdd/sgo-2021-01/Archive-2021-2025/
  — *Join:* VIN pattern — verified every row has a clean 11-character VIN prefix (WMI + VDS + check-digit position + year + plant), exactly the prefix ultravin already decodes; exact prefix match plus make/model/year fallback
  — *License:* US government work, public domain (NHTSA/ODI data). No restrictions on redistribution.
  — *Embed:* yes — current file is 1.7 MB raw / 208 KB gzipped (measured); full history with archive well under ~1.5 MB compressed · *Size:* ~1-1.5 MB gzipped including 2021-2025 archive · *Cadence:* Monthly — NHTSA posts updated CSVs monthly, matching ultravin's existing refresh automation exactly
  — *Why:* decode a VIN and see 'this exact vehicle line has N reported crashes with <system name> engaged, worst injury severity X' — unique per-VIN-pattern automation-crash exposure no other offline decoder offers; high demand from journalists, researchers, insurers, used-car shoppers
- **sgo-ads-crash-pack: robotaxi / automated-driving-system incident reports** *(interesting)*
  Incident reports for SAE Level 3-5 automated driving systems — Waymo Jaguar I-PACEs and Zeekrs, Zoox purpose-built vehicles, ADS trucks (Kenworth/Peterbilt), shuttles (Karsan, Ohmio). Verified 4,760 rows in the current rolling-year file with make, model, year, operating entity, injury severity, crash partner, and narrative. Distinct reporting rules from the ADAS file (ADS reports cover more incident types), so it deserves its own table.
  — *Source:* Same NHTSA SGO 2021-01 release, ADS file: https://static.nhtsa.gov/odi/ffdd/sgo-2021-01/SGO-2021-01_Incident_Reports_ADS.csv (+ SGO-2021-01_Incident_Reports_OTHER.csv, 33 rows)
  — *Join:* VIN pattern — verified 11-char VIN prefixes on 100% of rows (e.g. SADHW2S1* = Waymo I-PACE fleet); also make+model+year and operating-entity fields
  — *License:* US government work, public domain.
  — *Embed:* yes — 2.1 MB raw / 228 KB gzipped measured for the current year; full history under ~1.5 MB compressed · *Size:* ~1-1.5 MB gzipped including archive · *Cadence:* Monthly, same NHTSA release cycle as the ADAS file
  — *Why:* decoding any VIN from a robotaxi platform instantly reveals 'this VIN pattern is an ADS fleet vehicle (operator: Waymo/Zoox/...) with N reported incidents' — powers ex-robotaxi detection, AV-industry reporting, and per-platform incident profiles
- **ca-av-test-vehicle-registry: full-VIN ex-AV-test-car flag with autonomous miles** *(wild)*
  Every permitted AV test vehicle in California since 2015, listed by full VIN, with manufacturer, permit number, per-VIN monthly/annual autonomous miles, disengagement counts and causes, and driverless-capability flag. Verified: 2023 disengagement CSV contains full 17-char VINs (e.g. 4T1B21HK6KU514747) on every row, 12,692 disengagement records, 2 MB raw for one year.
  — *Source:* California DMV annual AV Disengagement Reports and Autonomous Mileage Reports (AVT + AVT Driverless programs), CSV per year: https://www.dmv.ca.gov/portal/vehicle-industry-services/autonomous-vehicles/disengagement-reports/ (e.g. https://www.dmv.ca.gov/portal/file/2023-autonomous-vehicle-disengagement-reports-csv/, https://www.dmv.ca.gov/portal/file/2022-autonomous-mileage-reports-csv/)
  — *Join:* FULL 17-character VIN — exact match, the strongest join key possible; aggregate to one row per VIN (manufacturer, years active, total autonomous miles, disengagement count)
  — *License:* California public records published for open download by CA DMV; factual data (not copyrightable). No explicit open-license grant — honest status: openly downloadable government records, redistribution universally practiced (Berkeley TIMS, academics).
  — *Embed:* yes — thousands of distinct VINs across all years; per-VIN aggregate table is a few hundred KB compressed · *Size:* ~200-500 KB gzipped (per-VIN aggregates); ~2-4 MB if raw disengagement rows kept · *Cadence:* Annual — reports due to DMV each Feb 1 covering Dec 1-Nov 30; new CSVs posted shortly after
  — *Why:* an 'ex-AV test vehicle' provenance flag: these Bolts, I-PACEs, Camrys and Pacificas eventually hit the used market with no title brand — ultravin would be the only offline tool that flags 'this exact VIN logged 8,412 autonomous test miles for Cruise LLC'
- **ca-ol316-collision-index: California AV collision report index (OL 316)** *(interesting)*
  Every reportable collision involving a permitted AV in California since 2019 (pre-2019 archived on request), filed as an OL 316 PDF within 10 days of the crash: date, manufacturer, vehicle year/make/model, autonomous mode engaged or conventional, location, damage, injuries, other-party details. Ship a parsed structured index plus deep link to each PDF.
  — *Source:* California DMV AV Collision Reports page — https://www.dmv.ca.gov/portal/vehicle-industry-services/autonomous-vehicles/autonomous-vehicle-collision-reports/ — 966 individual OL 316 PDF reports as of March 2026; UC Berkeley SafeTREC's TIMS AV Safety Dashboard (https://tims.berkeley.edu/tools/avsafety/) proves the PDFs parse into structured data
  — *Join:* make+model+year plus manufacturer/permit; cross-linkable to full VINs via the ca-av-test-vehicle-registry fleet lists (same permit holders); some OL316 forms include partial VIN fields
  — *License:* California public records, openly published; PDFs are factual government filings. Parsing pipeline required (one-time OCR/extract effort) — flag the engineering cost, not the legal one. From Jan 1 2028, AB 3061 legally requires DMV to publish these in open machine-readable format, which will delete the parsing cost entirely.
  — *Embed:* yes for the structured index (~1 MB); PDFs themselves stay online as links · *Size:* ~1-2 MB gzipped for the parsed index · *Cadence:* Page updated continuously as reports arrive (10-day filing deadline); monthly scrape fits the existing automation; becomes a clean machine-readable feed in 2028 by statute
  — *Why:* the ground-truth record of real AV crashes with per-incident detail SGO redacts — 'vehicles like this one appear in N California AV collision reports'; the 2028 open-format mandate makes building the pipeline now a bet that pays off automatically
- **cpuc-robotaxi-exposure: driverless passenger-service miles & incident denominators** *(interesting)*
  Quarterly per-vehicle and trip-level operations data from commercial robotaxi carriers: miles traveled in passenger service per vehicle, trips, passenger counts, collisions/citations/complaints, unplanned-stoppage events. This is the exposure denominator that turns SGO incident counts into incident *rates* — the number every researcher wants and can't easily get.
  — *Source:* California PUC AV Passenger Service Programs quarterly reporting — https://www.cpuc.ca.gov/regulatory-services/licensing/transportation-licensing-and-analysis-branch/autonomous-vehicle-programs/quarterly-reporting — ZIP archives of CSV/Excel filings from Waymo, Zoox (and historically Cruise)
  — *Join:* make+model+year at fleet level (Waymo = Jaguar I-PACE/Zeekr RT, Zoox = purpose-built); public files use vehicle IDs rather than VINs (some fields REDACTED under confidentiality claims) — no per-VIN join, disclose as fleet-level enrichment
  — *License:* California public records posted by CPUC; factual regulatory filings, openly downloadable; portions redacted as confidential. No explicit license — same honest status as other CA agency data.
  — *Embed:* partial — embed per-vehicle-quarter and per-platform aggregates (~1-5 MB); full trip-level data is too large and belongs in an optional extra or online adapter · *Size:* ~1-5 MB gzipped (aggregates only) · *Cadence:* Quarterly, fixed statutory deadlines (Feb 1, May 1, Aug 1, Nov 1)
  — *Why:* lets ultravin report 'this platform logged X million driverless passenger miles with Y incidents per million miles' next to the SGO crash counts — rates instead of raw tallies, which is the difference between data and journalism-bait
- **ntsb-automation-dockets: NTSB driving-automation investigation index** *(interesting)*
  A curated index of every NTSB highway investigation involving driving automation — the Uber ATG pedestrian fatality, the Tesla Autopilot HARs (Williston, Mountain View, Delray Beach), the Cruise pedestrian-dragging inquiry, and successors — with docket ID, vehicle make/model/year, automation system, probable cause, and links to the full docket and report PDFs. Small (dozens of cases) but the deepest-quality automation-crash analysis that exists.
  — *Source:* NTSB CAROL database, https://data.ntsb.gov/carol-main-public/ — public query UI with CSV/JSON export and documented API (https://data.ntsb.gov/carol-main-public/api-documentation); highway mode covered 2010-present; final reports like HAR-19/03 (Uber Tempe) and HAR-20/01 (Tesla Mountain View) at ntsb.gov
  — *Join:* make+model+year (+ automation system); docket documents frequently disclose the full VIN of the subject vehicle, which can be captured into the index where present
  — *License:* US government work, public domain (NTSB reports and dockets).
  — *Embed:* yes — trivially; under 100 KB · *Size:* <100 KB · *Cadence:* Sporadic (a few new highway automation investigations per year); CAROL API export makes the monthly refresh a cheap no-op most months
  — *Why:* when a decoded VIN matches a vehicle line NTSB formally investigated, surface 'NTSB investigated this platform's automation in HWY18MH010 — probable cause: ...' with a one-click docket link; catnip for journalists and litigators
- **iihs-automation-safeguards: partial-automation safeguard ratings (flagged licensing)** *(obvious)*
  IIHS's acceptable/marginal/poor grades for Level-2 system driver-monitoring safeguards — attention alerts, fail-safe behavior, cooperative steering. First round: 14 systems, 1 acceptable (Lexus Teammate), 2 marginal (GMC Sierra Super Cruise, Nissan Ariya ProPILOT 2.0), 11 poor (Tesla Autopilot/FSD, Ford BlueCruise, Mercedes, Volvo, Genesis, BMW). Rating identifies make, model, model-year range, and named system.
  — *Source:* IIHS partial driving automation safeguard ratings program, launched March 2024 — https://www.iihs.org/news/detail/first-partial-driving-automation-safeguard-ratings-show-industry-has-work-to-do and https://www.iihs.org/research-areas/advanced-driver-assistance (public test protocol PDF available)
  — *Join:* make+model+year + system name (same join style as the already-collected IIHS crash-test adapter)
  — *License:* IIHS content is copyrighted with no bulk-download or open-license grant — honest no-bake: ship as an online adapter/link-out, or at most a minimal facts-only rating table (uncopyrightable facts doctrine) clearly attributed; do NOT embed IIHS text or imagery
  — *Embed:* partial — the bare rating facts fit in <10 KB; full descriptions must stay online · *Size:* <10 KB (facts table) or zero (adapter) · *Cadence:* Irregular, a handful of new/updated system ratings per year as IIHS tests
  — *Why:* the only independent grade of whether a car's L2 system actually keeps the driver engaged — pairs brutally well next to that same system's SGO crash count in one decode output
- **adas-fingerprint: L2 system name & crash-history decoder derived from SGO** *(wild)*
  Flip the SGO data sideways: build a lookup of which named L2 system (Autopilot, BlueCruise, Super Cruise, SCC+LFA, ProPILOT Assist, Drive Pilot...) ships on which make/model/year and VIN prefix, harvested from thousands of manufacturer self-reports where they name the exact system and feature version on a specific VIN pattern. vPIC's ADAS fields say 'adaptive cruise: standard' — this says what the system is actually called and its reported crash count.
  — *Source:* Derived entirely from the public-domain SGO 2021-01 CSVs' 'Automation Feature Version' + make/model/year + VIN-prefix columns (https://static.nhtsa.gov/odi/ffdd/sgo-2021-01/SGO-2021-01_Incident_Reports_ADAS.csv), optionally cross-checked against manufacturer marketing-name conventions (AAA/PAVE 'Clearing the Confusion' naming work)
  — *Join:* VIN pattern (11-char prefix) and make+model+year, both directly present in the source rows
  — *License:* Public domain (derived from US government SGO data); any AAA/PAVE naming cross-reference is a small curated facts table
  — *Embed:* yes — an aggregation over data already proposed for embedding; the derived table itself is tiny · *Size:* <100 KB gzipped · *Cadence:* Monthly, free-riding on the same SGO refresh as ideas 1-2
  — *Why:* answers the question owners actually ask — 'what is this car's driver-assist system called and what's its track record?' — bridging vPIC's anonymous feature flags to real-world system identity; a genuinely novel derived dataset nobody publishes

### Auto finance (SEC ABS-EE securitization data)

- **abs-depreciation-curves: real-transaction vehicle values from auto ABS** *(interesting)*
  Every registered auto loan/lease ABS trust files monthly loan-level XML with <vehicleManufacturerName>, <vehicleModelName>, <vehicleModelYear>, <vehicleNewUsedCode>, <vehicleValueAmount> (value at origination), <vehicleValueSourceCode>, and <originationDate> — verified live in Ford Credit filings. Aggregating millions of used-vehicle originations by make+model+model-year and vehicle-age-at-origination yields empirical depreciation curves (median value at 1yr, 2yr, ... 8yr old) built from real financed transactions, not survey guesses; new-vehicle rows give real transaction values vs MSRP.
  — *Source:* SEC EDGAR Form ABS-EE EX-102 asset data files (Reg AB II Schedule AL), e.g. live example https://www.sec.gov/Archives/edgar/data/1813722/000181372224000012/autoloanmonthlydeal1021pool.xml (Ford Credit Auto Owner Trust 2020-B); enumerate via https://www.sec.gov/Archives/edgar/full-index/{year}/QTR{n}/form.idx (form type ABS-EE); spec: https://www.sec.gov/info/edgar/edgarabsxml.htm
  — *Join:* make+model+year (vehicleManufacturerName/vehicleModelName are free-text per originator — 'Escape', 'QX60 2WD' — so a normalization map to vPIC make/model IDs is required; filings contain no VINs for privacy)
  — *License:* US public records on SEC EDGAR; SEC 'Accessing EDGAR Data' page confirms all filings are free to access and download with no use restrictions (https://www.sec.gov/search-filings/edgar-search-assistance/accessing-edgar-data); fair-access rate limit of 10 req/s governs harvesting only. Derived aggregates are MIT-redistributable.
  — *Embed:* yes — the embedded output is an aggregate table (~30-60k make+model+year rows x age buckets, ~1-3 MB compressed). Honest caveat: the harvest pipeline is heavy (each trust's monthly EX-102 is 50-80 MB XML; historical backfill since Nov 2016 is hundreds of GB, monthly increment ~5-10 GB), but that runs in the refresh automation, not the wheel. · *Size:* 1-3 MB compressed aggregate table · *Cadence:* each trust files ABS-EE monthly (with its 10-D distribution report); new trusts appear continuously, old ones terminate via 15-15D — fits the existing monthly refresh automation exactly
  — *Why:* No free depreciation data exists at this fidelity — this is millions of actual financed-vehicle values. 'A 3-year-old Escape was worth 61% of new at loan origination' answers the #1 question buyers and fleet analysts ask a VIN decoder next.
- **abs-default-scorecard: loan delinquency/chargeoff rates by vehicle** *(wild)*
  Because each loan is re-reported every month until payoff/chargeoff, you can follow cohorts and compute per-make+model+year outcome rates: 60+ day delinquency incidence, chargeoff rate, repossession rate, payment-extension rate — optionally bucketed by <obligorCreditScore> tier to control for borrower mix. This is 'which vehicles do people stop paying for', a signal that exists nowhere else in public data.
  — *Source:* Same SEC EDGAR ABS-EE EX-102 auto loan files; performance fields verified live: <currentDelinquencyStatus>, <chargedoffPrincipalAmount>, <repossessedIndicator>, <zeroBalanceCode>, <zeroBalanceEffectiveDate>, <modificationTypeCode>, <paymentExtendedNumber> in https://www.sec.gov/Archives/edgar/data/1813722/000181372224000012/autoloanmonthlydeal1021pool.xml
  — *Join:* make+model+year (normalized vehicleManufacturerName/vehicleModelName + vehicleModelYear)
  — *License:* US public records, free EDGAR access, no use restrictions; aggregates freely redistributable (same as above)
  — *Embed:* yes — aggregate scorecard table ~30k rows x a dozen rates/counts, ~1-2 MB compressed. Must ship FICO-bucketed or shelf-labeled stats: coverage is only securitized loans (captives like Ford/GM/Toyota/Honda/Hyundai plus CarMax, Santander, World Omni), and credit mix varies wildly by shelf (Santander DRIVE is deep subprime), so raw pooled rates would mislead — publish per-credit-tier. · *Size:* 1-2 MB compressed · *Cadence:* monthly (each trust re-files loan-level performance every distribution period)
  — *Why:* A financial-distress fingerprint per vehicle: lenders, dealers, and buyers can see that (e.g.) subprime-financed Chargers charge off at multiples of Outbacks even within the same FICO band. Unique analytical content no valuation service publishes.
- **abs-repo-recovery: wholesale recovery severity after repossession** *(interesting)*
  For every charged-off loan the trust reports repossession proceeds and post-chargeoff recoveries against the charged-off principal. Aggregated by make+model+year and vehicle age, this yields recovery-rate curves — what a repossessed vehicle actually fetches at wholesale as a fraction of outstanding balance and of original vehicle value. Effectively a free, continuously updated wholesale-liquidation-value index.
  — *Source:* Same ABS-EE EX-102 auto loan files; fields verified live: <repossessedIndicator>, <repossessedProceedsAmount>, <recoveredAmount>, <chargedoffPrincipalAmount>
  — *Join:* make+model+year (normalized free-text vehicle fields)
  — *License:* US public records, free EDGAR access, no restrictions; aggregates MIT-redistributable
  — *Embed:* yes — smaller than the scorecard (only defaulted loans have these fields; still hundreds of thousands of events since 2016), ~0.5-1 MB compressed · *Size:* 0.5-1 MB compressed · *Cadence:* monthly, same ABS-EE pipeline
  — *Why:* Answers 'what is this vehicle worth in a forced sale' — the number lenders price risk with (Manheim data costs a fortune). Pairs with the default scorecard to give expected-loss per model.
- **lease-residual-truth: predicted vs actual lease-end values** *(wild)*
  Lease ABS filings state each vehicle's contract residual and independent base residual (e.g. ALG) at origination, then at termination report the termination type and actual liquidation proceeds. Aggregating terminated leases by make+model+year gives: how far actual lease-end values beat/missed forecast residuals, lessee buy-vs-return rates, and excess wear/mileage fee incidence — a residual-value accuracy index per model.
  — *Source:* SEC EDGAR ABS-EE EX-102 auto LEASE schema (namespace .../absee/autolease/assetdata); verified live in Nissan Auto Lease Trust 2019-B: https://www.sec.gov/Archives/edgar/data/1781522/000095013122000263/nalt19bex102_0114-1358.xml with <contractResidualValue>, <baseResidualValue>, <baseResidualSourceCode>, <liquidationProceedsAmount>, <terminationIndicator>, <acquisitionCost>, <excessFeeAmount>
  — *Join:* make+model+year (vehicleManufacturerName/vehicleModelName/vehicleModelYear, normalized; no VINs)
  — *License:* US public records, free EDGAR access, no restrictions; aggregates MIT-redistributable
  — *Embed:* yes — lease ABS universe is smaller (Nissan/Infiniti, BMW, Mercedes, VW/Audi, Hyundai, GM, Ford lease trusts), aggregate ~0.5-1 MB compressed · *Size:* 0.5-1 MB compressed · *Cadence:* monthly ABS-EE filings; termination events roll in continuously as 3-year leases mature
  — *Why:* Residual accuracy is the leasing industry's crown-jewel secret (ALG/Black Book sell it). 'Lessees bought out 71% of QX60s because actual value beat residual' tells a shopper whether a model's lease deals are priced against reality — and is a strong used-value signal.
- **financing-fingerprint: how each model actually gets financed** *(interesting)*
  Per make+model+year: median APR, term-length distribution (share of 72/84-month loans), loan-to-value at origination (negative-equity rolling shows up as LTV > 110%), payment-to-income, and new/used financing split. The 84-month-loan share and >100% LTV share per model are affordability-stress indicators invisible anywhere else.
  — *Source:* Same ABS-EE EX-102 auto loan + lease files; fields verified live: <originalInterestRatePercentage>, <originalLoanTerm>, <originalLoanAmount>, <vehicleValueAmount> (=> LTV), <paymentToIncomePercentage>, <vehicleNewUsedCode>, <originatorName>
  — *Join:* make+model+year (normalized free-text vehicle fields)
  — *License:* US public records, free EDGAR access, no restrictions; aggregates MIT-redistributable
  — *Embed:* yes — one aggregate table ~30k rows, ~1-2 MB compressed; same shelf-selection-bias caveat as the scorecard (captive-heavy universe) · *Size:* 1-2 MB compressed · *Cadence:* monthly ABS-EE pipeline
  — *Why:* Context a decoder can attach to any VIN: 'this model is typically financed 75 months at 128% LTV' is a debt-trap warning; also useful to journalists/analysts studying auto-credit stress by vehicle segment.
- **subvention-index: 0% APR captive-incentive intensity per model** *(wild)*
  Each loan/lease flags whether the manufacturer subsidized it (subvented rate/cash). The share of subvented and 0%-APR originations per make+model+year per origination quarter is a direct read on how hard the OEM had to prop up demand for that model over time — an incentive-desperation time series reconstructed from filings rather than ad monitoring.
  — *Source:* Same ABS-EE EX-102 files; fields verified live: <subvented> (rate and/or cash subvention codes) and <originalInterestRatePercentage> (the sampled Ford Escape loan was literally 0.00000000% APR, subvented)
  — *Join:* make+model+year (+ origination quarter), normalized free-text vehicle fields
  — *License:* US public records, free EDGAR access, no restrictions; aggregates MIT-redistributable
  — *Embed:* yes — small time-series table, well under 1 MB compressed; caveat: only visible for brands whose captives securitize (most majors do) · *Size:* <1 MB compressed · *Cadence:* monthly; new origination cohorts appear as new trusts file
  — *Why:* A demand-health signal per model year: heavy subvention historically precedes weak resale values. Gives ultravin users a 'was this model selling on its merits or on 0% financing?' flag.
- **buyer-profile-atlas: who finances each model, and where** *(interesting)*
  Per make+model+year: median and quartile buyer credit score, co-signer rate, income-verification mix, and the state distribution of financed vehicles. Yields both a buyer-credit profile ('median financed WRX buyer: 671 FICO, 18% co-signed') and a geographic demand map per model built from loan collateral states.
  — *Source:* Same ABS-EE EX-102 files; fields verified live: <obligorCreditScore>/<lesseeCreditScore>, <obligorCreditScoreType>, <coObligorIndicator>, <obligorIncomeVerificationLevelCode>, <obligorGeographicLocation> (state)
  — *Join:* make+model+year (normalized free-text vehicle fields); state dimension is obligor location
  — *License:* US public records, free EDGAR access, no restrictions; only aggregates ship (individual rows are already de-identified — no names, no VINs, state-level geography only)
  — *Embed:* yes — ~30k rows x score quantiles + 51-state share vectors; 2-4 MB compressed if state vectors are kept, smaller if top-5 states only · *Size:* 2-4 MB compressed · *Cadence:* monthly ABS-EE pipeline
  — *Why:* Fun and analytically real: rarity-by-state complements the existing NY-DMV rarity idea with 50-state coverage, and buyer credit profile explains the default scorecard's raw rates. Marketers, dealers, and car-culture writers will quote it.
- **captive-complaint-index: CFPB loan-servicing complaints mapped to brands** *(obvious)*
  All US consumer complaints about vehicle loans/leases with company, issue type (wrong repossession, payoff problems, GAP refund, credit reporting), state, and date. Aggregated per captive lender and mapped to the brands that captive finances, it yields a financing-experience score per make: complaints per year, repossession-dispute share, trend.
  — *Source:* CFPB Consumer Complaint Database bulk file https://files.consumerfinance.gov/ccdb/complaints.csv.zip (product 'Vehicle loan or lease'; landing: https://www.consumerfinance.gov/data-research/consumer-complaints/), joined through a hand-built captive-lender-to-brand map (Ford Motor Credit->Ford/Lincoln, GM Financial->GM brands, American Honda Finance->Honda/Acura, etc.)
  — *Join:* make only, via captive-lender->brand mapping (honest weakness: complaints identify the lender, not the vehicle; banks/credit unions that finance any brand can't be mapped, so this covers captive-financed loans only and joins at make granularity, not model/year)
  — *License:* US government work, public domain — CFPB publishes the database explicitly for free public use and bulk download
  — *Embed:* yes — aggregated per-lender/per-brand table is tiny (<100 KB); raw bulk file is ~1 GB zipped but stays in the pipeline · *Size:* <100 KB compressed · *Cadence:* CFPB refreshes the bulk file nightly; monthly snapshot is plenty
  — *Why:* Completes the financing-lifecycle story: after depreciation/default/residual stats, this tells a buyer what dealing with each brand's finance arm is actually like (Santander vs Toyota Financial complaint rates differ by an order of magnitude).

---

## Cross-cutting observations

1. **One normalization layer unlocks a dozen datasets.** Nearly every government source keys on
   free-text make/model. A single curated alias/normalization map (ODI text ↔ vPIC vocabulary)
   is the shared infrastructure for recalls, complaints, investigations, TSBs, FARS, theft rates,
   and fueleconomy.gov. Build it once, as data, with the same monthly-refresh discipline as vPIC.
2. **VIN prefixes are more common in government data than anyone assumes.** ODI complaints
   (84.7% of records), both SGO crash files (100%), NZ fleet data (positions 1–11), WA EV
   registrations (positions 1–10), CA AV registries (full 17). Datasets assumed to join on fuzzy
   make/model text often join at the exact granularity ultravin decodes.
3. **The decoder is its own join engine.** The FMCSA files carry 13.7M raw VINs; batch-decoding
   them with ultravin at refresh time (110k VIN/s makes this trivial) manufactures the
   make+model+year aggregation keys. Any VIN-bearing corpus becomes joinable data.
4. **Honesty about join granularity is the product.** Almost nothing joins 1:1 on full VIN; the
   output contract should carry a confidence tier ("applies to this exact pattern" / "may apply
   to this model-year") rather than pretending.
5. **Optional extras keep the core wheel lean.** Recurring pattern: a small aggregate baked into
   the core wheel, with the fat version (complaint narratives, full TSB set, truck provenance,
   opendbc pack) as separately-installable data packages. Note the hard limit verification found:
   PyPI's 100 MB per-file default rules out even "optional" narrative dumps — those are
   download-at-runtime territory.
6. **The VIN corpora are an asset in themselves.** FARS, ODI complaints, SGO, FMCSA, WA, and NZ
   registrations together yield tens of millions of real VINs — ground truth for CI validation,
   serial-ceiling estimates, and clone-sniff heuristics, regardless of whether any of it ships.
