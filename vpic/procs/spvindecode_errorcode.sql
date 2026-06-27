CREATE FUNCTION vpic.spvindecode_errorcode(vin character varying, modelyear integer, OUT err_returncode character varying, OUT err_correctedvin character varying, OUT err_errorbytes character varying, OUT err_unusedpositions character varying) RETURNS record
    LANGUAGE plpgsql
    AS $$
declare
	var_wmi varchar(6) = vpic.fVinWMI(vin);
	corrected varchar(17) = '';
	possibilities varchar(50) = '';
	replacements varchar(2000) = '';
	x varchar(50);
	i int = 3;
	n int = 14;
	var_c char(1);
	cntTotal int;
	cntMatch int;
	r varchar(50);
	cntErrors int = 0;
	lastErrorPos int = 0;
	lastReplacements varchar(50);
	tmpRowCount int = 0;
	current_c varchar = 0;
	tmpVin varchar(17);
	goodReplacements int = 0;
	NewReplacements varchar(50) = '';
	Corrected1 varchar(17);
	chr char(1);
	key varchar(100);
	b boolean = false;
	unUsedPos varchar(100) = '';
	ubound int = 11;
begin
	err_correctedvin = '';
	err_errorbytes = '';
	err_returncode = '';
	
	vin = trim(vin);

	if length(var_wmi) < 3 then
		err_returncode = err_returncode || ' 6 ';
	end if;

	create temporary table IF NOT EXISTS tbl_spVinDecode_ErrorCode (
        p int,
        c char(1)
    ) on commit drop;

	INSERT INTO tbl_spVinDecode_ErrorCode(p, c)
		SELECT DISTINCT position, "char"
		FROM vpic.WMIYearValidChars
		WHERE wmi = var_wmi
			AND year = modelYear
			AND var_wmi NOT IN (SELECT DISTINCT wmi FROM vpic.WMIYearValidChars_CacheExceptions)
		ORDER BY position, "char";

	SELECT COUNT(*) INTO tmpRowCount FROM tbl_spVinDecode_ErrorCode;
	if tmpRowCount = 0 then
		insert into tbl_spVinDecode_ErrorCode(p, c) select distinct p, c from vpic.fExtractValidCharsPerWmiYear(var_wmi, cast(modelYear as smallint)) order by p, c;
	end if;

	if length(var_wmi) = 6 then
		n = 11;
	end if;

	while (i < n) and (i < length(vin)) loop
		i = i + 1;
		var_c = substring(vin, i, 1);

		if i = 9 or i = 10 then
			r = var_c;
		else
			cntTotal = COUNT(*) from tbl_spVinDecode_ErrorCode where p = i;
			cntMatch = COUNT(*) from tbl_spVinDecode_ErrorCode where p = i and c = var_c;

			if cntTotal > 0 then
				if cntMatch > 0 then
					r = var_c;
				else
					r = '!';
					x = '';
					FOR current_c IN
						SELECT c
						FROM tbl_spVinDecode_ErrorCode
						WHERE p = i
						ORDER BY c
					LOOP
						x = x || current_c;
					END LOOP;

					replacements = replacements || '(' || CAST(i as varchar) || ':' || x || ')';
					cntErrors = cntErrors + 1;
					lastErrorPos = i;
					lastReplacements = x;
				end if;
			else
				r = var_c;
			end if;
		end if;

		corrected = corrected || r;
	end loop;

	if length(var_wmi) = 3 then
		corrected = var_wmi || corrected;
	else
		corrected = left(var_wmi, 3) || corrected || RIGHT(var_wmi, 3);
	end if;

	if length(vin) > length(corrected) then
		corrected = corrected || SUBSTRING(vin, length(corrected)+1, 3);
	end if;

	if cntErrors = 1 then
		if length(lastReplacements) = 1 then
			corrected = substring(vin, 1,lastErrorPos-1) || lastReplacements || substring(vin, lastErrorPos+1, 17-lastErrorPos);
			err_returncode = err_returncode || ' 2 ';
			err_correctedvin = Corrected;
			err_errorbytes = replacements;
		else
			i = 0;

			while i < length(lastReplacements) loop
				i = i + 1;

				var_c = SUBSTRING(lastReplacements, i, 1);
				tmpVin = substring(vin, 1, lastErrorPos-1) || var_c || substring(vin, lastErrorPos+1, 17-lastErrorPos);
				if SUBSTRING(tmpVin, 9, 1) = vpic.fVINCheckDigit(tmpVin) then
					goodReplacements = goodReplacements + 1;
					NewReplacements = NewReplacements || var_c;
					Corrected1 = tmpVin;
				end if;
			end loop;

			if goodReplacements = 1 then
				err_returncode = err_returncode || ' 3 ';
				err_correctedvin = Corrected1;
				err_errorbytes = '(' || CAST(lastErrorPos as varchar) || ':' || NewReplacements || ')';
			else
				err_returncode = err_returncode || ' 4 ';
				err_correctedvin = Corrected;
				err_errorbytes = '(' || CAST(lastErrorPos as varchar) || ':' || lastReplacements || ')';
			end if;
		end if;
	end if;

	if cntErrors > 1 then
		err_returncode = err_returncode || ' 5 ';
		err_correctedvin = Corrected;
		err_errorbytes = replacements;
	end if;

	create temporary table IF NOT EXISTS tbl_spVinDecode_ErrorCode1 (
        p int,
        c char(1)
    ) on commit drop;

	create temporary table IF NOT EXISTS tbl_spVinDecode_ErrorCodeY (
        p int,
        c char(1)
    ) on commit drop;

	i = (select min(o."id") from DecodingItems as o);

	while i <= (select max(o."id") from DecodingItems as o) loop
		key = null;
		key = (o.DecodingItem)."Keys" from DecodingItems as o where o."id" = i and (o.DecodingItem)."Source" ilike '%pattern%';

		if coalesce(key, '') <> '' then
			insert into tbl_spVinDecode_ErrorCode1 select * from vpic.fValidCharsInKey(key) where return_chr <> '|';
		end if;
		
		i = i + 1;
	end loop;

	insert into tbl_spVinDecode_ErrorCodeY select distinct * from tbl_spVinDecode_ErrorCode1;
	
	i = 3;
	if length(vin) < ubound then
		ubound = length(vin);
	end if;

	while i < ubound loop
		i = i + 1;

		if not i in (4, 5, 6, 7, 8, 11) then
			continue;
		end if;

		chr = SUBSTRING(vin, i, 1);
		b = false;

		if exists(select c from tbl_spVinDecode_ErrorCodeY where p +3 = i and c = chr) then
			b = true;
		end if;

		if b = false then
			unUsedPos = unUsedPos || ' ' || cast(i as varchar);
		end if;
	end loop;

	unUsedPos = replace(trim(unUsedPos), ' ', ',');

	if unUsedPos <> '' then
		err_returncode = err_returncode || ' 14 ';
		err_unusedpositions = unUsedPos;
	end if;

	drop table tbl_spVinDecode_ErrorCode;
	drop table tbl_spVinDecode_ErrorCode1;
	drop table tbl_spVinDecode_ErrorCodeY;
end;
$$;
