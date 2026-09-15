"""Generate a self-contained interactive native worker timeline."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Annotated, Any

import typer

from scripts.bench.end_to_end import ROOT

DEFAULT_OUTPUT = ROOT / "docs/figures/native-worker-timeline.html"


def _json_values(path: Path) -> list[Any]:
    text = path.read_text()
    try:
        return [json.loads(text)]
    except json.JSONDecodeError:
        return [json.loads(line) for line in text.splitlines() if line.strip()]


def _traces(value: Any, context: dict[str, Any] | None = None) -> list[dict[str, Any]]:
    context = dict(context or {})
    found: list[dict[str, Any]] = []
    if isinstance(value, dict):
        for name in ("label", "workers", "mode", "batch_rows", "benchmark", "trial"):
            if name in value:
                context[name] = value[name]
        trace = value.get("stage_trace")
        if isinstance(trace, dict):
            found.append({"context": context, "trace": trace})
        for name, child in value.items():
            if name != "stage_trace":
                found.extend(_traces(child, context))
    elif isinstance(value, list):
        for child in value:
            found.extend(_traces(child, context))
    elif isinstance(value, str) and value.lstrip().startswith(("{", "[")):
        try:
            found.extend(_traces(json.loads(value), context))
        except json.JSONDecodeError:
            for line in value.splitlines():
                try:
                    found.extend(_traces(json.loads(line), context))
                except json.JSONDecodeError:
                    continue
    return found


def _validate(profiles: list[dict[str, Any]]) -> list[dict[str, Any]]:
    profiles = [profile for profile in profiles if profile["trace"].get("events") != []]
    if not profiles:
        raise ValueError("input contains no sampled stage_trace events")
    for profile_index, profile in enumerate(profiles):
        trace = profile["trace"]
        if not isinstance(trace.get("events"), list):
            message = f"stage_trace {profile_index} has no events list"
            raise TypeError(message)
        for event_index, event in enumerate(trace["events"]):
            if not isinstance(event, dict):
                message = f"stage_trace {profile_index} event {event_index} must be an object"
                raise TypeError(message)
            required = ("stage", "worker", "rows", "start_ns", "end_ns")
            if any(name not in event for name in required):
                message = f"stage_trace {profile_index} event {event_index} is incomplete"
                raise ValueError(message)
            if (
                not isinstance(event["stage"], str)
                or (event["worker"] is not None and (not isinstance(event["worker"], int) or event["worker"] < 0))
                or not isinstance(event["rows"], int)
                or event["rows"] < 0
                or not isinstance(event["start_ns"], int)
                or not isinstance(event["end_ns"], int)
                or event["end_ns"] < event["start_ns"]
            ):
                message = f"stage_trace {profile_index} event {event_index} has invalid fields"
                raise ValueError(message)
        profile["id"] = str(profile_index)
        context = profile["context"]
        profile["label"] = (
            " · ".join(
                str(value)
                for value in (
                    f"{context['workers']} workers" if "workers" in context else None,
                    f"{context['batch_rows']:,} VINs/batch" if "batch_rows" in context else None,
                    f"trial {context['trial']}" if "trial" in context else None,
                )
                if value is not None
            )
            or context.get("label")
            or f"profile {profile_index + 1}"
        )
    return profiles


def _html(profiles: list[dict[str, Any]], sources: list[str]) -> str:
    payload = json.dumps({"profiles": profiles, "sources": sources}, separators=(",", ":")).replace("</", "<\\/")
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>UltraVIN native worker timeline</title><style>
:root{{--bg:#0b1020;--panel:#121a2e;--ink:#e8edf7;--muted:#9aa8c1;--grid:#29344d;--serial:#f59e0b;--decode:#38bdf8;--cleanup:#a78bfa;--other:#5eead4;--envelope:#64748b}}
*{{box-sizing:border-box}} body{{margin:0;background:var(--bg);color:var(--ink);font:14px system-ui,sans-serif}} main{{max-width:1500px;margin:auto;padding:22px}}
h1{{font-size:22px;margin:0 0 5px}} .note{{color:var(--muted);margin:0 0 16px}} .controls{{display:flex;flex-wrap:wrap;gap:12px;align-items:end;background:var(--panel);padding:12px;border-radius:9px}}
label{{display:grid;gap:4px;color:var(--muted);font-size:12px}} select,input,button{{background:#09101e;color:var(--ink);border:1px solid #34415e;border-radius:5px;padding:6px 8px}} button{{cursor:pointer}}
#summary{{margin:12px 0;color:var(--muted)}} #chart{{width:100%;height:auto;background:#0e1628;border-radius:8px}} .lane{{fill:#101a2e}} .grid{{stroke:var(--grid);stroke-width:1}} .label,.tick{{fill:var(--muted);font-size:11px}} .span{{stroke:#07101c;stroke-width:.5;cursor:crosshair}} .span:hover{{stroke:white;stroke-width:1.5}}
#tip{{position:fixed;display:none;pointer-events:none;background:#020617eF;border:1px solid #52617e;border-radius:6px;padding:8px;white-space:pre;z-index:3}} .legend{{display:flex;gap:14px;margin:10px 0;color:var(--muted)}} .swatch{{display:inline-block;width:10px;height:10px;margin-right:5px;border-radius:2px}}
</style></head><body><main><h1>Native worker timeline</h1>
<p class="note">Bars show task wall time, including time when the OS descheduled a worker.</p>
<div class="controls"><label>Profile<select id="profile"></select></label><label>Batch<select id="batch"></select></label><label>Window start (ms)<input id="start" type="number" min="0" step="0.1"></label><label>Window end (ms)<input id="end" type="number" min="0" step="0.1"></label><button id="apply">Apply window</button><button id="zoomIn">Zoom in</button><button id="zoomOut">Zoom out</button><button id="reset">Reset</button></div>
<div class="legend"><span><i class="swatch" style="background:var(--envelope)"></i>parallel wall envelope</span><span><i class="swatch" style="background:var(--serial)"></i>setup / sort / extraction</span><span><i class="swatch" style="background:var(--decode)"></i>decode worker</span><span><i class="swatch" style="background:var(--cleanup)"></i>cleanup worker</span><span><i class="swatch" style="background:var(--other)"></i>other</span></div>
<div id="summary"></div><svg id="chart" role="img" aria-label="Worker stage timeline"></svg><div id="tip"></div>
<script>const DATA={payload};
const $=id=>document.getElementById(id), profile=$('profile'), batch=$('batch'), svg=$('chart'), tip=$('tip'); let view;
for(const p of DATA.profiles){{const o=document.createElement('option');o.value=p.id;o.textContent=p.label;profile.append(o)}}
profile.value=DATA.profiles.find(p=>p.context.workers===Math.max(...DATA.profiles.map(p=>p.context.workers??0)))?.id||DATA.profiles[0].id;
function events(){{const p=DATA.profiles[+profile.value], b=batch.value;return p.trace.events.filter(e=>b==='all'||String(e.batch_id??e.batch??'unlabeled')===b)}}
function bounds(es){{if(!es.length)return [0,1];return [Math.min(...es.map(e=>e.start_ns)),Math.max(...es.map(e=>e.end_ns))]}}
function batches(){{const p=DATA.profiles[+profile.value], values=[...new Set(p.trace.events.map(e=>String(e.batch_id??e.batch??'unlabeled')))];batch.replaceChildren();for(const [v,t] of [['all','All sampled batches (gaps unknown)'],...values.map(v=>[v,`Batch ${{v}}`])]){{const o=document.createElement('option');o.value=v;o.textContent=t;batch.append(o)}}if(values.length)batch.value=values[Math.floor(values.length/2)];reset()}}
function syncWindow(){{$('start').value=(view[0]/1e6).toFixed(3);$('end').value=(view[1]/1e6).toFixed(3)}}
function reset(){{view=bounds(events());syncWindow();draw()}}
function role(e){{const s=e.stage.toLowerCase();if(s.endsWith('_parallel')||s.endsWith('_total'))return 'aggregate';if(e.worker==null||/setup|sort|extract|barrier|handoff/.test(s)||s==='cleanup_serial')return 'control';return 'worker'}}
function color(e){{const s=e.stage.toLowerCase(),r=role(e);if(r==='aggregate')return 'var(--envelope)';if(r==='control')return 'var(--serial)';if(s.includes('cleanup'))return 'var(--cleanup)';if(s.includes('decode'))return 'var(--decode)';return 'var(--other)'}}
function draw(){{const p=DATA.profiles[+profile.value], es=events(), preferred=['decode_total','decode_parallel','cleanup_total','cleanup_parallel'], present=[...new Set(es.filter(e=>role(e)==='aggregate').map(e=>e.stage))], aggregates=[...preferred.filter(s=>present.includes(s)),...present.filter(s=>!preferred.includes(s))], workers=Number.isInteger(p.context.workers)?Array.from({{length:p.context.workers}},(_,i)=>i):[...new Set(es.filter(e=>role(e)==='worker').map(e=>e.worker))].sort((a,b)=>a-b), pretty={{decode_total:'Total decode',decode_parallel:'Parallel decode',cleanup_total:'Total cleanup',cleanup_parallel:'Parallel cleanup'}}, lanes=[...aggregates.map(s=>pretty[s]??s),'Stage control',...workers.map(w=>`Worker ${{w}}`)];const W=1600,L=145,R=20,row=34,top=34,H=top+lanes.length*row+28,span=Math.max(1,view[1]-view[0]),x=n=>L+(n-view[0])/span*(W-L-R);svg.setAttribute('viewBox',`0 0 ${{W}} ${{H}}`);svg.replaceChildren();const add=(tag,a,text)=>{{const n=document.createElementNS('http://www.w3.org/2000/svg',tag);for(const [k,v] of Object.entries(a))n.setAttribute(k,v);if(text!=null)n.textContent=text;svg.append(n);return n}};lanes.forEach((name,i)=>{{add('rect',{{x:L,y:top+i*row,width:W-L-R,height:row,class:'lane',opacity:i%2?'.55':'.8'}});add('text',{{x:L-8,y:top+i*row+21,'text-anchor':'end',class:'label'}},name)}});for(let i=0;i<=10;i++){{const xx=L+i*(W-L-R)/10;add('line',{{x1:xx,x2:xx,y1:top-5,y2:H-25,class:'grid'}});add('text',{{x:xx,y:H-7,'text-anchor':'middle',class:'tick'}},`${{((view[0]+span*i/10)/1e6).toFixed(2)}} ms`)}}es.filter(e=>e.end_ns>=view[0]&&e.start_ns<=view[1]).forEach(e=>{{const r=role(e),lane=r==='aggregate'?aggregates.indexOf(e.stage):r==='control'?aggregates.length:aggregates.length+1+workers.indexOf(e.worker),y=top+lane*row+5,a=Math.max(view[0],e.start_ns),z=Math.min(view[1],e.end_ns),rect=add('rect',{{x:x(a),y,width:Math.max(1,x(z)-x(a)),height:row-10,rx:2,fill:color(e),class:'span'}});rect.onmousemove=ev=>{{tip.style.display='block';tip.style.left=ev.clientX+12+'px';tip.style.top=ev.clientY+12+'px';tip.textContent=`${{e.stage}}\n${{r==='worker'?`worker ${{e.worker}}`:r}} · ${{e.rows}} rows\n${{(e.start_ns/1e6).toFixed(3)}}-${{(e.end_ns/1e6).toFixed(3)}} ms (${{((e.end_ns-e.start_ns)/1e6).toFixed(3)}} ms)\nbatch ${{e.batch_id??e.batch??'unlabeled'}}`}};rect.onmouseleave=()=>tip.style.display='none'}});const gap=batch.value==='all'?' · unsampled gaps are unknown, not idle':'';$('summary').textContent=`${{p.label}} · ${{es.length}} events shown · ${{p.trace.dropped_events??0}} dropped · clock: ${{p.trace.clock??'unspecified'}} · sample every ${{p.trace.sample_every_batches??'unspecified'}} batches · per-bucket limit: ${{p.trace.events_per_bucket_limit??'unspecified'}}${{gap}}`;}}
profile.onchange=batches;batch.onchange=reset;$('apply').onclick=()=>{{const next=[Number($('start').value)*1e6,Number($('end').value)*1e6];if(next.every(Number.isFinite)&&next[0]>=0&&next[1]>next[0]){{view=next;draw()}}else syncWindow()}};$('zoomIn').onclick=()=>{{const m=(view[0]+view[1])/2,d=(view[1]-view[0])/4;view=[m-d,m+d];syncWindow();draw()}};$('zoomOut').onclick=()=>{{const m=(view[0]+view[1])/2,d=view[1]-view[0];view=[Math.max(0,m-d),m+d];syncWindow();draw()}};$('reset').onclick=reset;batches();
</script></main></body></html>"""


def main(
    inputs: Annotated[list[Path], typer.Argument(help="JSON or JSONL pipeline-probe captures")],
    output: Annotated[Path, typer.Option("--output", "-o")] = DEFAULT_OUTPUT,
) -> None:
    """Write an offline HTML worker timeline from one or more trace captures."""
    if not inputs:
        raise typer.BadParameter("at least one input is required", param_hint="inputs")
    missing = [path for path in inputs if not path.is_file()]
    if missing:
        message = f"file does not exist: {missing[0]}"
        raise typer.BadParameter(message, param_hint="inputs")
    profiles = _validate([trace for path in inputs for value in _json_values(path) for trace in _traces(value)])
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(_html(profiles, [str(path) for path in inputs]))
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
