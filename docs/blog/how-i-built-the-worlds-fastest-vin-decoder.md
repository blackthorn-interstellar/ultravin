# how i built the world's fastest VIN decoder

ok so this is a fun one. i built a VIN decoder that's ~1000x faster than the
official government one, and i shipped the whole vehicle database inside a 19.4 MB
`pip install`. no server, no network, no nothing. 38 microseconds per decode.

and here's the kicker: i didn't write a single line. claude code wrote all of it in
ultracode mode. i just pointed it at the right problems and pressed enter.

let me show you how, because the *method* is more interesting than the code.

## first: what's the actual problem

a VIN is those 17 characters on your car. decoding it = turning that into "2003 Honda
Accord, 4 cylinder, built in blah." the canonical decoder is NHTSA's vPIC. it's
great. it's also the truth, legally speaking.

but here's how you're "supposed" to use it: download a 1.5 GB database dump, stand up
a SQL Server or Postgres instance, load the dump, and call a stored procedure called
`spVinDecode`. every month there's a new dump.

think about that. gigabytes of infra, a running database server, monthly ops — to
answer a question that takes 17 characters in and one row out. that's insane. so the
whole project starts with one realization:

> **decoding isn't a database problem.** it's: look up the manufacturer code → pick
> the schema by model year → pattern match → rank the attributes. that's it.

so we don't host the database. we bake the data AND the logic of `spVinDecode`
straight into one little Rust file and decode in memory. no SQL engine at runtime.
the file *is* the product. this is the whole "no code beats clever code" thing — the
smartest move was deleting the entire database.

## lesson 1: build the judge before you build the thing

this is the most important part so i'm putting it first.

before writing any decoder, i had claude stand up the *real* vPIC in Postgres in
docker and call the real `spVinDecode`. that's the "oracle" — the source of truth.

why does this matter so much? because now "is my decoder correct?" isn't an opinion
you argue about. it's a question you ask a database, a few hundred thousand times.

and i set the bar absurdly high on purpose: not "95% accurate" (which is what the
best open-source decoder claims). **byte-for-byte identical** to the government one,
or it's a bug. we copy vPIC exactly, we don't "improve" it. when correctness is
measurable, you can chase it. when it's vibes, you can't.

## lesson 2: ship it ugly, then make it right, then make it fast

i did this in three passes. literally three "workflows" in the git log.

**pass 1 — make it decode anything.** ~2700 lines of Rust. manufacturer lookup, year
filtering, pattern matching, attribute resolution. it decoded VINs. it was wrong in a
hundred little ways. didn't care yet. it ran.

**pass 2 — make it match the oracle exactly.** this is where the boring-but-critical
stuff lives: the check-digit math, the suggested-VIN error correction, the error
codes. and a generator that walks *every* manufacturer, *every* schema, *every*
pattern, plus broken VINs, and checks every field against Postgres.

fun detail: vPIC has one easy-to-miss tiebreak deep in its dedup — it breaks ties
by row `id`, deterministically. there's nothing random about it, but miss the `id
ASC` order and a handful of VINs resolve to the wrong attribute. so we mirror that
exact order and wrote down why. that's the kind of thing you only hit because the
oracle caught it.

**pass 3 — make it fast.** zero-copy loading (the embedded database loads basically
for free), caching, batching. but here's the rule that made it work: **every speed
optimization had to still pass the parity check or it didn't ship.** correctness was
never up for negotiation once we had it.

## lesson 3: don't write tests, torture it

your test suite only covers the bugs you already thought of. to find the ones you
*didn't*, i built a thing called `brutal` and just... attacked the decoder with it.

first run: pushed ~220,000 VINs through both my decoder and the real one. found
**44,713 disagreements.** deduped down to 26 actual bug types.

then i made it smarter — a coverage-guided fuzzer that uses my fast decoder as the
signal and only sends the *interesting* VINs to the slow Postgres oracle. catalog
grew to 35 bug types. every single one was in the weird error/correction path. clean
VINs were already perfect. the bugs were stuff like "Rust uppercases unicode
differently than SQL does." nobody writes that test by hand.

then i went big: **5 Postgres oracles running in parallel**, one engine marching
backward through every model year from 2027 to 1980 enumerating everything, the
fuzzer running till it ran out of new ground. multi-day run, fully checkpointed, with
a detached supervisor so it survived me closing my laptop. total: **134,661
disagreements found, crushed down to 35 signatures.**

the result is my favorite part. **exact parity on every decodable VIN except two —
and in both, the GOVERNMENT decoder is the one that's wrong.** one VIN literally
*crashes* Postgres (a busted regex range). the other, the oracle reads a stale cache
that was frozen 22 minutes before a schema got added to the *same dump*, so it
contradicts itself. mine doesn't. you can't have "parity" with a crash, so i
documented those two as "we're more correct than the reference" and moved on.

## the numbers

after the speed pass: warm decode went **4204 µs → 38.4 µs.** cold start (fresh
process, load the whole db, decode one VIN) went **29.3 ms → 0.753 ms.**

VINs per second, same machine, same test corpus:

| engine | VIN/s |
|---|---|
| **mine, batched across 4 cores** | **94,030** |
| **mine, 1 core** | **29,568** |
| best open-source (corgi v3) | ~83 |
| NHTSA SQL Server | 22.5 |
| NHTSA Postgres | 19.5 |
| NHTSA web API (rate limited) | ~10 |

~1000x faster than the SQL procedures it copies. whole database in a 19.4 MB wheel.
`pip install ultravin`, done. it's live on PyPI.

## the takeaways (the actual point of this post)

1. **build the judge first.** a Postgres instance that disagrees with you beats any
   amount of "looks right to me." make correctness a number, not a feeling.
2. **right before fast, always.** and never trade one back for the other. i even
   missed one speed target *on purpose* because hitting it risked breaking parity.
   not worth it.
3. **fuzz, don't hand-write tests.** the machine found two bugs in the *government's*
   decoder. you were never going to write those tests.
4. **the best code is no code.** the win wasn't a clever algorithm. it was deleting
   the database entirely and shipping the answer.

and the meta-lesson: i didn't write this. an agent did, in a weekend, with me steering.
the skill now isn't typing the code — it's knowing *what to point it at*. building the
oracle, setting the bar, demanding the fuzzer. that's the job now.

clear, fast, boring. ship it. 🚀
