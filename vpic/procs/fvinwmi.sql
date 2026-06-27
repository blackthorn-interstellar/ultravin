CREATE FUNCTION vpic.fvinwmi(vin character varying) RETURNS character varying
    LANGUAGE plpgsql
    AS $$
declare
	wmi varchar(6);
begin

	if length(vin) > 3 then
		wmi = left(vin, 3);
	else
		wmi = vin;
	end if;

	if substring(wmi, 3, 1) = '9' and length(vin) >= 14 then
		wmi = wmi || substring(vin, 12, 3);
	end if;

	return wmi;
end;
$$;
