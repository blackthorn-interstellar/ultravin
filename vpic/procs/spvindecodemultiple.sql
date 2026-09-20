CREATE FUNCTION vpic.spvindecodemultiple(vin_list character varying[]) RETURNS TABLE(vin character varying, manufacturer character varying, make character varying, model character varying, modelyear character varying, "trim" character varying, series character varying, cleandecode boolean)
    LANGUAGE plpgsql
    AS $$
declare
	v_len integer;
	v_max_limit constant integer := 100;
begin
	if vin_list is null then
		return;
	end if;

	v_len = cardinality(vin_list);

	if v_len = 0 then
		return;
	end if;

	if v_len > v_max_limit then
		raise exception 'Input array exceeds maximum allowed size of % elements (received %)', v_max_limit, v_len
		using errcode = 'array_subscript_error';
	end if;

	return query
	select
		u.vin::character varying(17) as vin,
		max(case when d.variable = 'Manufacturer Name' then d.value end)::character varying as manufacturer,
		max(case when d.variable = 'Make' then d.value end)::character varying as make,
		max(case when d.variable = 'Model' then d.value end)::character varying as model,
		max(case when d.variable = 'Model Year' then d.value end)::character varying as modelyear,
		max(case when d.variable = 'Trim' then d.value end)::character varying as "trim",
		max(case when d.variable = 'Series' then d.value end)::character varying as series,
		btrim(max(case when d.variable = 'Error Code' then d.value end)) in ('0', '0,10', '1,10', '1,400', '1,10,400') as cleandecode
	from unnest(vin_list) with ordinality as u(vin, ord)
	left join lateral vpic.spvindecode(v := u.vin) as d on true
	group by u.vin, u.ord
	order by u.ord;
end;
$$;
