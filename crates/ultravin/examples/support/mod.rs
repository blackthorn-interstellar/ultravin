use ultravin::DecodeResult;

pub fn native_output_bytes(results: &[DecodeResult<'_>], result_capacity: usize) -> usize {
    use std::mem::size_of;

    const SAMPLE_ROWS: usize = 16;
    let outer = result_capacity * size_of::<DecodeResult<'_>>();
    if results.is_empty() {
        return outer;
    }
    let samples = results.len().min(SAMPLE_ROWS);
    let sampled_bytes = if samples == 1 {
        native_row_owned_bytes(&results[0])
    } else {
        (0..samples)
            .map(|sample| sample * (results.len() - 1) / (samples - 1))
            .map(|index| native_row_owned_bytes(&results[index]))
            .sum()
    };
    outer + sampled_bytes * results.len() / samples
}

fn native_row_owned_bytes(result: &DecodeResult<'_>) -> usize {
    use std::borrow::Cow;
    use std::mem::size_of;

    let mut bytes = result.vin.capacity()
        + result.wmi.capacity()
        + result.descriptor.capacity()
        + result.corrected_vin.capacity()
        + result.error_codes.capacity() * size_of::<i32>()
        + result.elements.capacity() * size_of::<ultravin::DecodedElement<'_>>();
    for element in &result.elements {
        for text in [
            &element.value,
            &element.attribute_id,
            &element.keys,
            &element.source,
        ] {
            if let Cow::Owned(owned) = text {
                bytes += owned.capacity();
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::mem::size_of;

    fn result_with_source(source: Cow<'static, str>) -> DecodeResult<'static> {
        DecodeResult {
            vin: String::with_capacity(19),
            wmi: String::with_capacity(5),
            descriptor: String::with_capacity(11),
            model_year: None,
            error_codes: Vec::with_capacity(3),
            check_digit_valid: true,
            corrected_vin: String::with_capacity(23),
            elements: vec![ultravin::DecodedElement {
                group_name: "group",
                variable: "variable",
                value: Cow::Owned(String::with_capacity(7)),
                element_id: 1,
                attribute_id: Cow::Owned(String::with_capacity(13)),
                code: "code",
                data_type: "string",
                decode: "decode",
                source,
                pattern_id: None,
                vin_schema_id: None,
                keys: Cow::Owned(String::with_capacity(17)),
                created_on: None,
                wmi_id: None,
                to_be_qced: false,
            }],
        }
    }

    #[test]
    fn native_memory_sampling_is_exact_for_uniform_rows() {
        let results: Vec<_> = (0..17)
            .map(|_| result_with_source(Cow::Borrowed("static source")))
            .collect();
        let expected = results.capacity() * size_of::<DecodeResult<'_>>()
            + results.len() * native_row_owned_bytes(&results[0]);
        assert_eq!(native_output_bytes(&results, results.capacity()), expected);
    }

    #[test]
    fn native_memory_excludes_borrowed_element_text() {
        let mut result = result_with_source(Cow::Borrowed("Pattern"));
        let owned = native_row_owned_bytes(&result);
        result.elements[0].value = Cow::Borrowed("value");
        result.elements[0].attribute_id = Cow::Borrowed("123");
        result.elements[0].keys = Cow::Borrowed("ABCDE");
        assert_eq!(owned - native_row_owned_bytes(&result), 7 + 13 + 17);
    }

    #[test]
    fn native_memory_counts_owned_source_but_excludes_borrowed_source() {
        let borrowed = result_with_source(Cow::Borrowed("static source"));
        let owned = result_with_source(Cow::Owned(String::with_capacity(29)));
        assert_eq!(
            native_row_owned_bytes(&owned) - native_row_owned_bytes(&borrowed),
            29
        );
    }
}
