"""Run complete ultravin WASM instances concurrently on Metal with a shared database."""

from __future__ import annotations

import struct
import sys
import time
from array import array
from contextlib import ExitStack
from functools import lru_cache
from pathlib import Path

import wasmgpu
import wgpu
from wasmgpu.compiler import vm_support

Row = tuple[str, int | None]
PAGE_WORDS = 1024
STACK_SLOTS = 4096
CALL_DEPTH = 128
MEMORY_PAGES = 2048
QUANTUM = 8192
WARMUP_VIN = b"1HGCM82633A004352"
STATE_WORDS = 16  # wasmgpu 0.0.2 State layout


class BatchCapacityError(RuntimeError):
    """The shared Metal buffers cannot accommodate this complete batch."""


class BatchTimeoutError(TimeoutError):
    """The requested wall-time limit expired between Metal dispatches."""


def _shader(compiled_blocks: str | None = None) -> str:
    # Reuse the pinned runtime's validator, control flow and scalar operations,
    # including its software f64. Only the address-space backing is different.
    root = Path(wasmgpu.__file__).parent
    vm = vm_support() if compiled_blocks is not None else (root / "vm.wgsl").read_text()
    start = vm.index("fn read_heap(")
    end = vm.index("fn bounds(", start)
    vm = vm[:start] + vm[end:]
    scalar = (root / "operations.wgsl").read_text()
    if compiled_blocks is not None:
        scalar = "struct NumericResult { value: vec2u, trap: u32 }"
        vm += "\n" + compiled_blocks + "\n" + (root / "compiled.wgsl").read_text()
    return "\n".join(
        [
            (root / "numeric.wgsl").read_text(),
            scalar,
            vm.replace("@workgroup_size(64)", "@workgroup_size(32)"),
            Path(__file__).with_name("batch.wgsl").read_text(),
        ]
    )


class MetalDecoder:
    def __init__(
        self,
        wasm: Path,
        max_input: int,
        now: int,
        warmup_result: bytes,
        *,
        compiled: bool = False,
        batch_timeout: float | None = None,
    ) -> None:
        self.batch_timeout = batch_timeout
        with ExitStack() as resources:
            self._prepare(resources, wasm, max(max_input, len(WARMUP_VIN)), now, warmup_result, compiled)
            self.resources = resources.pop_all()

    def _prepare(
        self, resources: ExitStack, wasm: Path, max_input: int, now: int, warmup_result: bytes, compiled: bool
    ) -> None:
        module = wasmgpu.Module(wasm, execution="interpreter")
        if any(fn.imported is not None for fn in module._binary.functions):
            raise ValueError("the Metal guest must be self-contained")
        self.max_input = max_input
        self.now = now
        self.entry = module._binary.exports["decode_row"][1]
        # Warm the real indexes once ON THE GPU. The single-VIN API does not
        # cache answers. Every timed lane starts here and executes decode_row.
        seed = resources.enter_context(
            module.spawn(
                1, memory_pages=1650, stack_size=STACK_SLOTS, call_depth=CALL_DEPTH, fuel=150_000_000, quantum=32768
            )
        )
        self.adapter = dict(seed.adapter_info)
        if self.adapter["backend_type"] != "Metal":
            msg = f"expected Metal, got {self.adapter}"
            raise RuntimeError(msg)
        self.device = seed._context.device
        self.program = seed._program_buffer
        single = seed._batches[0]
        self.image = single.buffers[3]
        self.memory_offset = seed._memory_offset
        self.entry_tables = (seed._table_offset, seed.table_elements, seed._data_flags, seed._element_flags)
        self.binding_limit = self.device.limits["max-storage-buffer-binding-size"]
        entries = [
            {
                "binding": i,
                "visibility": wgpu.ShaderStage.COMPUTE,
                "buffer": {"type": "uniform" if i == 1 else "read-only-storage" if i in (0, 7) else "storage"},
            }
            for i in range(9)
        ]
        self.layout = self.device.create_bind_group_layout(entries=entries)
        self.pipeline_layout = self.device.create_pipeline_layout(bind_group_layouts=[self.layout])
        shader = self.device.create_shader_module(code=_shader())
        self.pipelines = {
            name: self.device.create_compute_pipeline(
                layout=self.pipeline_layout, compute={"module": shader, "entry_point": name}
            )
            for name in ("start_rows", "run", "gather")
        }
        (pointer,) = seed.call("reserve_input", [max_input])
        seed.write_memory(pointer, WARMUP_VIN)
        (size,) = seed.call("decode_vin", [(len(WARMUP_VIN), 0, 0, now)])
        (address,) = seed.call("output_ptr")
        if seed.read_memory(address, size) != warmup_result:
            raise RuntimeError("GPU and native warm-up outputs differ")
        (self.input_ptr,) = seed.call("reserve_input", [max_input])
        self.initial_pages = single.state[5]
        self.plan = None
        if compiled:
            self.plan = wasmgpu.Module(wasm, execution="compiled").compiled
            # The compiled program uses jump tables and function metadata. Its
            # table addresses refer to the same initialized guest image.
            metadata = array("I", self.plan.program)
            metadata[4] = len(metadata)
            for i, (offset, capacity) in enumerate(zip(seed._table_offsets, seed._table_capacities, strict=True)):
                metadata.extend((seed._table_lengths + i, offset, capacity, 0))
            self.program = self.device.create_buffer_with_data(data=metadata, usage=wgpu.BufferUsage.STORAGE)
            resources.callback(self.program.destroy)
        self.compilations = 0
        self.dispatches = 0
        self.guest_instructions = 0
        self.batch_retries = 0
        self.peak_lanes = 0
        self.native_pipeline = lru_cache(maxsize=128)(self._compile_region)
        resources.callback(self.native_pipeline.cache_clear)

    def _compile_region(self, region: int):
        assert self.plan is not None
        shader = self.device.create_shader_module(code=_shader(self.plan.source_for(region)))
        self.compilations += 1
        return self.device.create_compute_pipeline(
            layout=self.pipeline_layout, compute={"module": shader, "entry_point": "run"}
        )

    def close(self) -> None:
        self.resources.close()

    def _group(self, buffers: list):
        return self.device.create_bind_group(
            layout=self.layout,
            entries=[
                {"binding": i, "resource": {"buffer": b, "offset": 0, "size": b.size}} for i, b in enumerate(buffers)
            ],
        )

    def _submit(self, group, stages) -> None:
        encoder = self.device.create_command_encoder()
        for stage, groups in stages:
            compute = encoder.begin_compute_pass()
            compute.set_pipeline(self.pipelines[stage] if isinstance(stage, str) else self.native_pipeline(stage))
            compute.set_bind_group(0, group)
            compute.dispatch_workgroups(groups)
            compute.end()
        self.device.queue.submit([encoder.finish()])
        self.dispatches += len(stages)

    def _inputs(self, rows: list[Row]) -> bytes:
        count = len(rows)
        vins = [vin.encode() for vin, _ in rows]
        width = max(map(len, vins))
        if width > self.max_input:
            raise ValueError("input exceeds the reserved guest input length")
        stride = 2 + (width + 7) // 8
        out = bytearray((count + 3 + count * stride) * 8)
        struct.pack_into("<IIIIq", out, count * 8, self.entry, self.initial_pages, self.input_ptr, stride, self.now)
        for i, (vin, (_, year)) in enumerate(zip(vins, rows, strict=True)):
            start = (count + 3 + i * stride) * 8
            struct.pack_into("<IIi", out, start, len(vin), int(year is not None), year or 0)
            out[start + 16 : start + 16 + len(vin)] = vin
        return bytes(out)

    def decode(self, rows: list[Row]) -> list[bytes]:
        if not rows:
            return []
        try:
            result = self._batch(rows)
        except BatchCapacityError:
            if len(rows) == 1:
                raise
            self.batch_retries += 1
            middle = len(rows) // 2
            return self.decode(rows[:middle]) + self.decode(rows[middle:])
        self.peak_lanes = max(self.peak_lanes, len(rows))
        return result

    def _batch(self, rows: list[Row]) -> list[bytes]:
        started = time.monotonic()
        count = len(rows)
        logical_pages = (self.memory_offset + MEMORY_PAGES * 16384 + PAGE_WORDS - 1) // PAGE_WORDS
        directory_size = (1 + logical_pages * count) * 4
        if directory_size > self.binding_limit or count * STACK_SLOTS * 8 > self.binding_limit:
            raise BatchCapacityError("GPU address-space directory or stack is too large")
        config = array(
            "I",
            [
                count,
                STACK_SLOTS,
                CALL_DEPTH,
                MEMORY_PAGES,
                self.memory_offset,
                *self.entry_tables,
                QUANTUM,
                1,
                0,
                0,
                0,
                0,
                0,
            ],
        )
        queue = self.device.queue
        with ExitStack() as stack:

            def buffer(size: int, data=None, *, uniform: bool = False):
                usage = wgpu.BufferUsage.UNIFORM if uniform else wgpu.BufferUsage.STORAGE
                result = self.device.create_buffer(
                    size=size, usage=usage | wgpu.BufferUsage.COPY_SRC | wgpu.BufferUsage.COPY_DST
                )
                stack.callback(result.destroy)
                if data is not None:
                    queue.write_buffer(result, 0, data)
                return result

            inputs = self._inputs(rows)
            if len(inputs) > self.binding_limit:
                raise BatchCapacityError("packed inputs exceed the GPU storage buffer limit")
            # Splitting can concentrate a large schema build into one lane.
            # Let a single VIN use any writable page in its logical memory.
            private_bytes = min(
                self.binding_limit, MEMORY_PAGES * 65536 if count == 1 else (8 * 1024 + count * 512) * 1024
            )
            states = buffer(count * STATE_WORDS * 4)
            io = buffer(len(inputs), inputs)
            buffers = [
                self.program,
                buffer(64, config, uniform=True),
                buffer(count * STACK_SLOTS * 8),
                buffer(private_bytes),
                buffer(count * CALL_DEPTH * 16),
                states,
                io,
                self.image,
                buffer(directory_size),
            ]
            group = self._group(buffers)
            groups = (count + 31) // 32
            self._submit(group, [("start_rows", groups)])
            stages = [("run", groups)] * 4 if self.plan is None else []
            last_compiled = -1
            next_update = time.monotonic() + 60
            while True:
                if stages:
                    self._submit(group, stages)
                state = queue.read_buffer(states).cast("I")
                statuses = state[7::STATE_WORDS].tolist()
                if 2 in statuses:
                    row = statuses.index(2)
                    trap = state[row * STATE_WORDS + 8]
                    if trap == 250:
                        raise BatchCapacityError("GPU private page pool exhausted")
                    msg = f"GPU trap {trap} in row {row}"
                    raise RuntimeError(msg)
                if all(s == 1 for s in statuses):
                    if self.plan is not None and (any(state[6::STATE_WORDS]) or not all(state[11::STATE_WORDS])):
                        message = "guest instructions did not all use compiled pipelines"
                        raise RuntimeError(message)
                    # Per-lane fuel is below 2^32, so these counters fit in one word.
                    instruction = 6 if self.plan is None else 11
                    self.guest_instructions += sum(state[instruction::STATE_WORDS])
                    break
                if self.batch_timeout is not None and time.monotonic() - started >= self.batch_timeout:
                    executed = sum(state[6::STATE_WORDS]) + sum(state[11::STATE_WORDS])
                    msg = (
                        f"timed out with {statuses.count(1)}/{count} rows complete and {executed:,} guest instructions"
                    )
                    raise BatchTimeoutError(msg)
                if self.plan is not None:
                    regions = {self.plan.region_for(state[i * STATE_WORDS]) for i, s in enumerate(statuses) if s == 0}
                    upcoming = [(region, groups) for region in sorted(regions)]
                    executed = sum(state[11::STATE_WORDS])
                    # Generating a pipeline can split its region. A changed
                    # continuation is legitimate even before any lane advances.
                    if executed == last_compiled and upcoming == stages:
                        raise RuntimeError("compiled Metal dispatch made no progress")
                    last_compiled, stages = executed, upcoming
                    if time.monotonic() >= next_update:
                        print(
                            f"Metal has compiled {self.compilations} pipelines and executed "
                            f"{executed:,} guest instructions across {count} rows",
                            file=sys.stderr,
                            flush=True,
                        )
                        next_update = time.monotonic() + 60

            # The guest returns its actual JSON pointer and length. Lay out the
            # readback after those are known so variable-size results never truncate.
            pointers = bytes(queue.read_buffer(io, 0, count * 8))
            meta = bytearray(count * 16)
            meta[: count * 8] = pointers
            sizes = [size for size, _ in struct.iter_unpack("<II", pointers)]
            end = count * 2
            for i, size in enumerate(sizes):
                struct.pack_into("<I", meta, (count + i) * 8, end)
                end += (size + 7) // 8
            if end * 8 > self.binding_limit:
                raise BatchCapacityError("JSON output exceeds the GPU storage buffer limit")
            packed = buffer(end * 8, meta)
            buffers[6] = packed
            self._submit(self._group(buffers), [("gather", count)])
            data = queue.read_buffer(packed, count * 16)
            output = []
            start = 0
            for size in sizes:
                output.append(bytes(data[start : start + size]))
                start += (size + 7) // 8 * 8
        return output
