# Worker slot-budget sweep

The immutable `slot_budget_probe` swept `batch_size × slots_per_worker` while setting the live-result cap to exactly `workers × batch_size × slots_per_worker`. Every observation used the fixed 20,000,000-row unique corpus, a complete warm pass followed by a complete timed pass, ordered delivery and full result materialization. Timed passes include owner cleanup; persistent workers are joined after the timed pass. The production auto comparison additionally includes worker startup and shutdown.

## 12-worker grid

| Batch | S1 | S2 | S5 | S10 |
|---:|---:|---:|---:|---:|
| 100 | 910,005 | 945,406 | **961,448** | 942,917 |
| 200 | 887,306 | 943,377 | 942,676 | 956,335 |
| 400 | 874,224 | 921,613 | 946,747 | 959,363 |

Rates are VIN/s. A single observation does not establish a meaningful difference among B100/S5, B200/S10, and B400/S10, which were within 0.6%. The alternating controls supported the narrower selection between B100/S5 and B200/S5:

| Pair | B200/S5 | B100/S5 | B100 advantage |
|---:|---:|---:|---:|
| 1 | 943,428 | 957,429 | 1.5% |
| 2 | 956,954 | 979,123 | 2.3% |

B100/S5 is the supported twelve-worker default from this comparison. Ten slots helped B200 and B400 in their single observations, but B400/S10 raised peak RSS to 2.221 GB versus 2.007 GB for B100/S5, about 214 MB, while its measured rate was slightly lower.

## Selected worker-width cases

| Workers | B100/S5 | B200/S2 | B200/S5 | B400/S2 |
|---:|---:|---:|---:|---:|
| 8 | 843,852 | 841,184 | **845,172** | 823,082 |
| 4 | **492,493** | 478,356 | 473,278 | 458,530 |

The W8 B100/S5 and B200/S5 results differ by only 0.2%, so they are empirically tied at this measurement precision. The four W4 observations declined monotonically in run order; this run order cannot separate batch-size effects from machine-load or thermal drift. The B100/S5 lead needs a reversed comparison before treating it as a stable four-worker optimum. These are selected cases, not full W4/W8 surfaces.

The W1 B200/S5 reference measured **134,166 VIN/s**, with 1.003 average busy cores.

## Evidence

- Primary progressive evidence: `scripts/bench/slot_budget_sweep_2026_09_15.json`
- Alternating controls and W1: `scripts/bench/slot_budget_followup_2026_09_15.json`
- Immutable binary SHA-256: `5bb4ae1af99fc91b2743cc298a1619248406b6c7964c6bb084ddc4b70bef149a`
- Immutable source SHA-256: `8fb1133a8059f3e4943e2be555db379dfb936b4f094f62b672ebc872454d4aee`
- Corpus SHA-256: `0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a`

Peak RSS includes the roughly 2 GB process footprint of the loaded 20-million-row corpus, so it should be used for within-sweep comparisons rather than interpreted as slot storage alone. The probe's recorded peak live rows remained at or below each configured cap in every run.
