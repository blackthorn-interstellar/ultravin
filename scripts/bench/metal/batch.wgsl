// Each VIN gets a mutable WASM address space. An untouched page (including the
// 80 MiB vPIC archive) is read directly from the same initialized GPU image.
@group(0) @binding(7) var<storage, read> image: array<u32>;
struct PageMap {
    allocated: atomic<u32>,
    entries: array<u32>,
}
@group(0) @binding(8) var<storage, read_write> pages: PageMap;
const PAGE_WORDS = 1024u;

fn image_word(index: u32) -> u32 {
    if index < arrayLength(&image) { return image[index]; }
    return 0u;
}
fn read_heap(index: u32) -> u32 {
    let page = pages.entries[(index / PAGE_WORDS) * config.count + lane];
    if page == 0u { return image_word(index); }
    return heap[(page - 1u) * PAGE_WORDS + index % PAGE_WORDS];
}
fn write_heap(index: u32, value: u32) {
    let key = (index / PAGE_WORDS) * config.count + lane;
    var page = pages.entries[key];
    if page == 0u {
        let next = atomicAdd(&pages.allocated, 1u);
        if next >= arrayLength(&heap) / PAGE_WORDS { fail(250u); return; }
        let source = index - index % PAGE_WORDS;
        for (var i = 0u; i < PAGE_WORDS; i++) {
            heap[next * PAGE_WORDS + i] = image_word(source + i);
        }
        page = next + 1u;
        pages.entries[key] = page;
    }
    heap[(page - 1u) * PAGE_WORDS + index % PAGE_WORDS] = value;
}

// This Rust guest has no WASI imports. Charge bulk memory instructions just
// as the interpreter does, and surface an unexpected service call as a trap.
fn wasi_dispatch(_service: u32, _base: u32) -> u32 { fail(251u); return 0u; }
fn fs_charge(amount: u32) -> bool {
    if !fuel_available(amount) { fail(8u); return false; }
    consume_fuel(amount);
    return true;
}

@compute @workgroup_size(32)
fn start_rows(@builtin(global_invocation_id) id: vec3u) {
    lane = id.x;
    if lane >= config.count { return; }
    let header = config.count;
    let entry = output[header];
    let input = output[header + 1u];
    let clock = output[header + 2u];
    let row = header + 3u + lane * input.y;
    let length_year = output[row];
    let function = 16u + entry.x * 8u;
    vm.pc = program[function];
    vm.sp = program[function + 3u];
    vm.function_id = entry.x;
    vm.pages = entry.y;
    vm.fuel = 500000000u;
    for (var i = 0u; i < length_year.x; i++) {
        let bytes = output[row + 2u + i / 8u];
        write_byte(input.x + i, (bytes[(i / 4u) % 2u] >> ((i % 4u) * 8u)) & 255u);
    }
    // The buffers start zeroed, including the entry function's other locals.
    set_value(0u, vec2u(length_year.x, 0u));
    set_value(1u, vec2u(length_year.y, 0u));
    set_value(2u, vec2u(output[row + 1u].x, 0u));
    set_value(3u, clock);
    states[lane] = vm;
}

fn json_word(address: u32, remaining: u32) -> u32 {
    let index = config.memory_offset + address / 4u;
    let shift = (address % 4u) * 8u;
    var word = read_heap(index) >> shift;
    if shift != 0u && remaining > 4u - address % 4u {
        word |= read_heap(index + 1u) << (32u - shift);
    }
    return word;
}

// One threadgroup per result copies the actual UTF-8 bytes into input order.
@compute @workgroup_size(64)
fn gather(@builtin(workgroup_id) group: vec3u, @builtin(local_invocation_index) thread: u32) {
    lane = group.x;
    let result = output[lane];
    let destination = output[config.count + lane].x;
    for (var pair = thread; pair * 8u < result.x; pair += 64u) {
        let address = result.y + pair * 8u;
        let remaining = result.x - pair * 8u;
        var words = vec2u(json_word(address, remaining), 0u);
        if remaining > 4u { words.y = json_word(address + 4u, remaining - 4u); }
        output[destination + pair] = words;
    }
}
