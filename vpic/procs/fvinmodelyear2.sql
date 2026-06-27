CREATE FUNCTION vpic.fvinmodelyear2(vin character varying) RETURNS integer
    LANGUAGE plpgsql
    AS $_$
declare
	pos10 char(17);
	modelYear int = null;
	conclusive boolean = false;
	var_wmi varchar(6) = null;
	vehicleTypeId int = null;
	truckTypeId int = null;
	carLT int = 0;
begin
	vin = upper(vin);

	if length(vin) >= 10 then
		pos10 = substring(vin, 10, 1);

		if pos10 BETWEEN 'A' AND 'H' THEN
			modelYear = 2010 + ascii(pos10) - ASCII('A');
		end if;

		if pos10 BETWEEN 'J' AND 'N' THEN
			modelYear = 2010 + ascii(pos10) - ASCII('A') -1;
		end if;

		if pos10 = 'P' THEN
			modelYear = 2023;
		end if;

		if pos10 BETWEEN 'R' AND 'T' THEN
			modelYear = 2010 + ascii(pos10) - ASCII('A') -3;
		end if;

		if pos10 BETWEEN 'V' AND 'Y' THEN
			modelYear = 2010 + ascii(pos10) - ASCII('A') -4;
		end if;

		if pos10 BETWEEN '1' AND '9' THEN
			modelYear = 2031 + ascii(pos10) - ASCII('1');
		end if;
	end if;

	if modelYear is not null then
		var_wmi = vpic.fVinWMI(vin);
		if var_wmi is not null then
			select vpic.Wmi.vehicleTypeId, vpic.Wmi.truckTypeId into vehicleTypeId, truckTypeId from vpic.Wmi where vpic.Wmi.wmi = var_wmi;

			if vehicleTypeId in (2, 7) or (vehicleTypeId = 3 and truckTypeId = 1) then
				carLT = 1;
			end if;

			if (carLT = 1) and (substring(vin, 7, 1) ~ '^[0-9]$') then
				modelYear = modelYear - 30;
				conclusive = true;
			end if;

			if (carLT = 1) and (substring(vin, 7, 1) ~ '^[A-Z]$') then
				conclusive = true;
			end if;

			if modelYear > EXTRACT(YEAR FROM (NOW() + INTERVAL '2 years')) then
				modelYear = modelYear - 30;
				conclusive = true;
			end if;
		end if;
	end if;

	if conclusive <> true then
		modelYear = - modelYear;
	end if;
	
	return modelYear;
end;
$_$;
