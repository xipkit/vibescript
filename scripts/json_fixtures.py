"""API payloads with independent results, kept outside the recorded goldens."""
import json


def benchmark_cases():
    cases = []
    row_type = "{ active: bool, id: int, name: string, score: float, tags: array<string>, detail: { city: string, note: string } }"
    packet_type = "{ cursor: string, records: array<" + row_type + "> }"

    def encode(value):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))

    def add(name, body, argument, expected, param, result):
        for metered in [True, False]:
            cases.append(dict(
                name=name + ("/metered" if metered else "/unlimited"),
                source=f"def run(input: {param}) -> {result}\n{body}\nend",
                args=[argument], expected=expected, accounting=metered, iterations=100,
            ))

    for label, size in [("1k", 1024), ("16k", 16384), ("256k", 262144), ("1m", 1048576)]:
        rows = []
        used = len(encode({"cursor": "next-page", "records": []}).encode())
        while True:
            i = len(rows)
            row = dict(active=i % 3 != 0, id=i, name=f"record-{i:06}", score=i * 1.25,
                       tags=["api", "ready"], detail=dict(city="Montréal 東京", note='line one\n"quoted" \\ route'))
            extra = len(encode(row).encode()) + bool(rows)
            if used + extra > size:
                break
            rows.append(row)
            used += extra
        packet = dict(cursor="next-page", records=rows)
        raw = encode(packet)
        assert size * 0.8 < len(raw.encode()) <= size
        add(f"json_api_parse_{label}", "JSON.parse(input)", raw, packet, "string", "any")
        add(f"json_api_shape_{label}", f"JSON.parse_as(input, {packet_type})", raw, packet, "string", packet_type)
        add(f"json_api_array_{label}", f"JSON.parse_as(input, array<{row_type}>)", encode(rows), rows, "string", f"array<{row_type}>")
        projected = [dict(id=row["id"], label=row["name"], score=row["score"] * 2.0) for row in rows if row["active"]]
        body = (f"rows=JSON.parse_as(input, array<{row_type}>)\n"
                'selected=rows.select { |row| row["active"] }\n'
                'output=selected.map { |row| { id: row["id"], label: row["name"], score: row["score"] * 2.0 } }\n'
                'JSON.stringify(output)')
        # Vibescript omits .0 from integral floats when stringifying.
        for row in projected:
            if row["score"].is_integer():
                row["score"] = int(row["score"])
        add(f"json_api_project_{label}", body, encode(rows), encode(projected), "string", "string")
        if size >= 16384:
            serialized = json.loads(raw)
            for row in serialized["records"]:
                if row["score"].is_integer():
                    row["score"] = int(row["score"])
            add(f"json_api_stringify_{label}", "JSON.stringify(input)", packet, json.dumps(serialized, ensure_ascii=False, separators=(",", ":"), sort_keys=True), packet_type, "string")
    return cases
