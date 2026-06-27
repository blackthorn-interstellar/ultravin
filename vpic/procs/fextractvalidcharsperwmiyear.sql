CREATE FUNCTION vpic.fextractvalidcharsperwmiyear(input_wmi character varying, year smallint) RETURNS TABLE(p smallint, c character)
    LANGUAGE plpgsql
    AS $$
declare
	keys varchar(50);
	DECLARE cursor_wmiy CURSOR FOR
		SELECT distinct p.Keys
		FROM 
			vpic.Wmi AS w 
			INNER JOIN vpic.Wmi_VinSchema AS wvs ON w.Id = wvs.WmiId 
			INNER JOIN vpic.VinSchema AS vs ON wvs.VinSchemaId = vs.Id 
			INNER JOIN vpic.Pattern AS p ON vs.Id = p.VinSchemaId
		WHERE     
			(w.Wmi = input_wmi)
			and year between wvs.YearFrom and COALESCE(wvs.YearTo, 2999);
begin
    create temporary table IF NOT EXISTS tbl_fExtractValidCharsPerWmiYear (
        p smallint,
        c char(1)
    ) on commit drop;
	
	OPEN cursor_wmiy;

	LOOP
		FETCH NEXT FROM cursor_wmiy INTO keys;
		
		EXIT WHEN NOT FOUND;

		insert into tbl_fExtractValidCharsPerWmiYear(p, c) select pos + 3, return_chr from vpic.fValidCharsInKey(keys);
	END LOOP;

	CLOSE cursor_wmiy;
	
	return query select * from tbl_fExtractValidCharsPerWmiYear;
	drop table tbl_fExtractValidCharsPerWmiYear;
end;
$$;
