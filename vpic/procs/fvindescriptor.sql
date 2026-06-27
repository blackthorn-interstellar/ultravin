CREATE FUNCTION vpic.fvindescriptor(vin character varying) RETURNS character varying
    LANGUAGE plpgsql
    AS $$
declare
	vehicleDescriptor varchar(17);
begin
	vin = LEFT(TRIM(vin) || '*****************', 17);
	vin = SUBSTRING(vin, 1, 8) || '*' || SUBSTRING(vin, 10);

	vehicleDescriptor = LEFT(vin, 11);
	if SUBSTRING(vin, 3, 1) = '9' then
		vehicleDescriptor = left(vin, 14);
	end if;
	
	return upper(vehicleDescriptor);
end;
$$;
