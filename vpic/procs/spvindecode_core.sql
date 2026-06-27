CREATE FUNCTION vpic.spvindecode_core(pass integer, modelyear integer, var_vin character varying DEFAULT ''::character varying, modelyearsource character varying DEFAULT ''::character varying, conclusive boolean DEFAULT false, error12 boolean DEFAULT false, includeall boolean DEFAULT NULL::boolean, includeprivate boolean DEFAULT false, includenotpublicilyavailable boolean DEFAULT NULL::boolean) RETURNS TABLE(coredecodingid integer, corecreatedon timestamp without time zone, corepatternid integer, corekeys character varying, corevinschemaid integer, corewmiid integer, coreelementid integer, coreattributeid character varying, corevalue character varying, coresource character varying, corepriority integer, coretobeqced boolean, corereturncode character varying)
    LANGUAGE plpgsql
    AS $_$
declare
	ReturnCode varchar(100);
	var_wmi varchar(6) = vpic.fVinWMI(var_vin);
	var_keys varchar(50) = '';
	wmiId integer;
	patternId integer;
	vinSchemaId integer;
	formulaKeys varchar(14) = '';
	cnt integer = 0;
	descriptor varchar(17) = vpic.fVinDescriptor(var_vin);
	CorrectedVIN varchar(17);
	ErrorBytes varchar(500);
	AdditionalDecodingInfo varchar(500);
	UnUsedPositions varchar(500);
	EngineModel varchar(500);
	k varchar(50);
	MfrId int;
	MfrName varchar(500);
	var_modelId integer;
	fromElementId integer;
	toElementId integer;
	formula varchar;
	params varchar;
	sql varchar;
	value varchar(500);
	conversionId integer;
	dataType varchar(50);
	cursor_item RECORD;
	result varchar(500) = '';
	tVehicleType integer;
	isOffRoad boolean = false;
	vehicleType varchar(500);
	isVinExceptionCheckDigit boolean = false;
	invalidChars varchar(500) = '';
	startPos integer = 13;
	x_vehicleTypeId integer;
	x_truckTypeId integer;
	j integer = 0;
	chr varchar(10) = '';
	isCarMpvLT boolean = false;
	CD char(1);
	calcCD char(1) = '';
	errors varchar(100);
	offRoadNote varchar(100) = ' NOTE: Disregard if this is an off-road vehicle PIN, as check digit calculation may not be accurate.';
	checkDigitExclusionNote varchar(150) = ' NOTE: Check Digit Exception - The check digit was given an exception based on data from the OEM indicating an error on production.';
	errorMessages varchar = null;
	errorCodes varchar(500) = null;
	oneError varchar(10) = '';
begin
	ReturnCode = '';

	if length(var_vin) > 3 then
		var_keys = SUBSTRING(var_vin, 4, 5);
		if length(var_vin) > 9 then
			var_keys  = var_keys || '|' || SUBSTRING(var_vin, 10, 8);
		end if;
	end if;

	-- NOTE: Unable to directly insert into custom type via select statement, needs to store it in a temp table as a column
	create temporary table IF NOT EXISTS DecodingItems (
		id SERIAL PRIMARY KEY,
		DecodingItem vpic."tblDecodingItem"
	) on commit drop;

	select Id into wmiId from vpic."wmi" where "wmi" = var_wmi and (includeNotPublicilyAvailable = true or PublicAvailabilityDate <= NOW());
	if wmiId is null then
		ReturnCode = ReturnCode || ' 7 ';
		CorrectedVIN = '';
		ErrorBytes = '';
	else
		insert into DecodingItems (DecodingItem) select ROW(
			null,
			pass,
			coalesce(P.UpdatedOn, P.CreatedOn),
			P.Id,
			upper(P.Keys),
			P.VinSchemaId,
			wvs.WmiId,
			P.ElementId,
			P.AttributeId,
			'XXX',
			'Pattern',
			wvs.YearFrom,
			vs.TobeQCed)::vpic."tblDecodingItem"
			FROM
				vpic.Pattern AS P
				INNER JOIN vpic.Element E ON P.ElementId = E.Id
				INNER JOIN vpic.VinSchema VS on p.VinSchemaId = vs.Id
				INNER JOIN vpic.Wmi_VinSchema AS wvs ON vs.Id = wvs.VinSchemaId and ((modelYear is null) or (modelYear between wvs.YearFrom and coalesce(wvs.YearTo, 2999))) 
				INNER JOIN vpic.Wmi AS w ON wvs.WmiId = w.Id and w.Wmi = var_wmi
			WHERE (
				    (p.keys NOT LIKE '%[%' AND var_keys LIKE replace(p.keys, '*', '_') || '%')
				    OR (p.keys LIKE '%[%' AND var_keys ~ p.keys_regex)
				  )
				and not P.ElementId in  (26, 27, 29, 39)
				and not E.Decode is null
				and (coalesce(E.IsPrivate, false) = false or includePrivate = coalesce(E.IsPrivate, false))
				and (includeNotPublicilyAvailable = true or (w.PublicAvailabilityDate <= NOW()))
				and (includeNotPublicilyAvailable = true or (coalesce(vs.TobeQCed, false) = false))
				ORDER BY P.Id ASC;
		
		SELECT (di.DecodingItem)."AttributeId", (di.DecodingItem)."PatternId", (di.DecodingItem)."VinSchemaId", (di.DecodingItem)."Keys"
		INTO EngineModel, patternId, vinSchemaId, k
		FROM DecodingItems di
		WHERE (di.DecodingItem)."DecodingId" = pass AND (di.DecodingItem)."ElementId" = 18
		ORDER BY (di.DecodingItem)."Priority" DESC, (di.DecodingItem)."CreatedOn" DESC, di.id DESC
		LIMIT 1;

		if not EngineModel is null then
			insert into DecodingItems (DecodingItem) select ROW(
			null, pass, coalesce(p.UpdatedOn, p.CreatedOn),
			patternId, k, vinSchemaId, wmiId, p.ElementId,
			p.AttributeId, 'XXX', 'EngineModelPattern', 50, null)::vpic."tblDecodingItem"
			from
				vpic.EngineModel em
				inner join vpic.EngineModelPattern AS p on em.Id = p.EngineModelId
				INNER JOIN vpic.Element E ON P.ElementId = E.Id
			where
				lower(trim(em.Name)) = lower(trim(EngineModel));
		end if;
	
		insert into DecodingItems (DecodingItem) select ROW(
		null, pass, coalesce(w.UpdatedOn, w.CreatedOn),
		null, upper(var_wmi), null, w.Id, 39,
		cast(t.Id as character varying), upper(t.Name), 'VehType', 100, null)::vpic."tblDecodingItem"
		from vpic.wmi w
			join vpic.VehicleType t on t.Id = w.VehicleTypeId
		where w.wmi = var_wmi
			and (includeNotPublicilyAvailable = true or (w.PublicAvailabilityDate <= NOW()));
	
		select t.id, upper(t.name) into MfrId, MfrName
		from vpic.wmi w
		join vpic.Manufacturer t ON t.id = w.ManufacturerId
		where w.wmi = var_wmi and (includeAll = true or (w.PublicAvailabilityDate <= NOW()));
	
		insert into DecodingItems (DecodingItem) select ROW(
		null, pass, null, null, upper(var_wmi), null, WmiId, 27, cast(MfrId as character varying), MfrName, 'Manuf. Name', 100, null)::vpic."tblDecodingItem";
	
		insert into DecodingItems (DecodingItem) select ROW(
		null, pass, null, null, upper(var_wmi), null, WmiId, 157, cast(MfrId as character varying), cast(MfrId as character varying), 'Manuf. Id', 100, null)::vpic."tblDecodingItem";
	
		insert into DecodingItems (DecodingItem) select ROW(
		null, pass, null, null, modelYearSource, null, null, 29,
		cast(modelYear as character varying), cast(modelYear as character varying), 'ModelYear', 100, null)::vpic."tblDecodingItem"
		where not modelYear is null;
	
		formulaKeys = var_keys;
		formulaKeys = replace(formulaKeys, cast(1 as text), '#');
		formulaKeys = replace(formulaKeys, cast(2 as text), '#');
		formulaKeys = replace(formulaKeys, cast(3 as text), '#');
		formulaKeys = replace(formulaKeys, cast(4 as text), '#');
		formulaKeys = replace(formulaKeys, cast(5 as text), '#');
		formulaKeys = replace(formulaKeys, cast(6 as text), '#');
		formulaKeys = replace(formulaKeys, cast(7 as text), '#');
		formulaKeys = replace(formulaKeys, cast(8 as text), '#');
		formulaKeys = replace(formulaKeys, cast(9 as text), '#');
		formulaKeys = replace(formulaKeys, cast(0 as text), '#');
	
		insert into DecodingItems (DecodingItem) select ROW(
			null, pass, coalesce(p.UpdatedOn, p.CreatedOn), p.Id, p.Keys, p.VinSchemaId, null, p.ElementId, p.AttributeId, SUBSTRING(var_keys, STRPOS(p.keys, '#'), (LENGTH(p.keys) - STRPOS(REVERSE(p.Keys), '#') + 1) - (STRPOS(p.keys, '#')) + 1), 'Formula Pattern', 100, null)::vpic."tblDecodingItem"
		from vpic.Pattern as p INNER JOIN vpic.Element E ON p.ElementId = E.Id 
		where
			p.VinSchemaId in (
				select wvs.VinSchemaId from vpic.Wmi as w
				inner join vpic.Wmi_VinSchema as wvs on w.Id = wvs.WmiId and ((modelYear is null) or (modelYear between wvs.YearFrom and coalesce(wvs.YearTo, 2999)))
				where w.Wmi = var_wmi and ((modelYear is null) or (modelYear between wvs.YearFrom and coalesce(wvs.YearTo, 2999)))
			)
			and STRPOS(p.keys, '#') > 0
			and not p.ElementId in (26, 27, 29, 39)
			and formulaKeys like replace(p.Keys, '*', '_') || '%';
	
		DELETE FROM DecodingItems
		WHERE id IN (
		    SELECT id
		    FROM (
		        SELECT 
		            d.id,
		            RANK() OVER (
		                PARTITION BY (d.DecodingItem)."ElementId" 
		                ORDER BY 
		                    (d.DecodingItem)."Priority" DESC, 
		                    (d.DecodingItem)."CreatedOn" DESC, 
		                    LENGTH(REPLACE(COALESCE((d.DecodingItem)."Keys", ''), '*', '')) ASC, 
		                    REPLACE(REPLACE(COALESCE((d.DecodingItem)."Keys", ''), '[', ''), ']', '') ASC,
		                    d.id ASC
		            ) AS RankResult
		        FROM DecodingItems d
		        WHERE (d.DecodingItem)."DecodingId" = pass 
		        AND (d.DecodingItem)."ElementId" NOT IN (121, 129, 150, 154, 155, 114, 169, 186)
		    ) t 
		    WHERE t.RankResult > 1
		);
	
		var_modelId = (di.DecodingItem)."AttributeId" FROM DecodingItems di
		WHERE (di.DecodingItem)."DecodingId" = pass AND (di.DecodingItem)."ElementId" = 28;
	
		if not var_modelId is null then
			insert into DecodingItems (DecodingItem) select ROW(
			null, pass, null,
			(di.DecodingItem)."PatternId", (di.DecodingItem)."Keys", (di.DecodingItem)."VinSchemaId", null, 26,
			mk.Id, upper(mk.name), 'pattern - model', 1000, null)::vpic."tblDecodingItem"
			from
				vpic.Make_Model mm
				inner join vpic.Make AS mk on mm.MakeId = mk.Id
				inner join DecodingItems as di on mm.ModelId = cast((di.DecodingItem)."AttributeId" as integer) and (di.DecodingItem)."DecodingId" = pass
			where
				(di.DecodingItem)."ElementId" = 28 and (di.DecodingItem)."DecodingId" = pass;
		else
			cnt = count(*)
			from vpic.wmi w
				join vpic.Wmi_Make wm on wm.WmiId = w.Id
				join vpic.Make t on t.Id = wm.MakeId
			where Wmi = var_wmi
				and (includeNotPublicilyAvailable = true or (PublicAvailabilityDate <= NOW()));

			if cnt = 1 then
				insert into DecodingItems (DecodingItem) select ROW(
				null, pass, coalesce(w.UpdatedOn, w.CreatedOn),
				null, var_wmi, null, w.Id, 26,
				cast(t.Id as character varying), upper(t.Name), 'Make', -100, null)::vpic."tblDecodingItem"
				from vpic.wmi w
					join vpic.Wmi_Make wm on wm.WmiId = w.Id
					join vpic.Make t on t.Id = wm.MakeId
				where wmi = var_wmi
					and (includeNotPublicilyAvailable = true or (w.PublicAvailabilityDate <= NOW()));
			end if;
		end if;
	
		FOR cursor_item IN
	        SELECT
	            (di.DecodingItem)."Keys",
	            (di.DecodingItem)."ElementId",
	            (di.DecodingItem)."AttributeId",
	            c.ToElementId,
	            c.Formula,
	            c.id,
	            e.DataType,
	            (di.DecodingItem)."PatternId",
	            (di.DecodingItem)."VinSchemaId",
	            (di.DecodingItem)."WmiId"
	        FROM DecodingItems di
	        INNER JOIN vpic.conversion c ON (di.DecodingItem)."ElementId" = c.FromElementId
	        INNER JOIN vpic.Element e ON c.ToElementId = e.Id
	        WHERE (di.DecodingItem)."DecodingId" = pass
	        ORDER BY (di.DecodingItem)."Priority" DESC, (di.DecodingItem)."CreatedOn" DESC, c.id
	    LOOP
	        var_keys = cursor_item."Keys";
			fromElementId = cursor_item."ElementId";
			value = cursor_item."AttributeId";
			toElementId = cursor_item.ToElementId;
			formula = cursor_item.Formula;
			conversionId = cursor_item.Id;
			dataType = cursor_item.DataType;
			patternId = cursor_item."PatternId";
			vinschemaId = cursor_item."VinSchemaId";
			wmiId = cursor_item."WmiId";
	
			if not exists (select 1 from DecodingItems di where (di.DecodingItem)."DecodingId" = pass and (di.DecodingItem)."ElementId" = toElementId) then
				formula = replace(formula, '#x#', value);
		
				if lower(dataType) = 'decimal' then
					dataType = dataType || '(12, 2)';
				end if;
		
				if lower(dataType) = 'int' then
					dataType = dataType || 'cast(round(' || formula || ',0))';
				end if;
		
				sql = 'select (' || formula || ')::varchar(500)';
		
				begin
					execute sql into result;
				exception
					when others then
						result = '0';
				end;
		
				insert into DecodingItems (DecodingItem) select ROW(
				null, pass, null, patternId, var_keys, vinschemaId, wmiId, toElementId, result, result,
				left('Conversion ' || CAST(conversionId as varchar) || ': ' || formula, 50), 100, null)::vpic."tblDecodingItem";
			end if;
	    END LOOP;

		select (di.DecodingItem)."AttributeId" into tVehicleType from DecodingItems di 
		where (di.DecodingItem)."DecodingId" = pass and (di.DecodingItem)."ElementId" = 39 limit 1;

		create temporary table IF NOT EXISTS tbl_tmpPatterns (
	        id int,
	        TobeQCed boolean
    	) on commit drop;

		create temporary table IF NOT EXISTS tbl_tmpPatternsEx (
	        id int,
	        a int,
			b int
    	) on commit drop;

		insert into tbl_tmpPatterns(id, tobeqced)
		select distinct sp.id, s.TobeQCed
		from vpic.VehicleSpecSchema as s
			inner join vpic.VSpecSchemaPattern as sp on s.id = sp.SchemaId
			inner join vpic.VehicleSpecPattern p on sp.Id = p.VSpecSchemaPatternId
			inner join vpic.VehicleSpecSchema_Model as vssm on vssm.VehicleSpecSchemaId = s.id
			left outer join vpic.VehicleSpecSchema_Year as vssy on vssy.VehicleSpecSchemaId = s.id
			inner join vpic.Wmi_Make wm on wm.MakeId = s.makeid
			inner join vpic.wmi on wmi.id = wm.WmiId
		where 1 = 1
			and wmi.wmi = var_wmi
			and s.VehicleTypeId = tVehicleType
			and vssm.ModelId = var_modelId
			and (vssy.Year = modelYear or vssy.Id is null) 
			and p.IsKey = true
			and (includeNotPublicilyAvailable = true or (coalesce(s.TobeQCed, false) = false));
  
		insert into tbl_tmpPatternsEx (id, a, b) 
		select
			p.VSpecSchemaPatternId, count(*) as cntTotal, count(distinct d.id) as cntMatch
		from
			vpic.VehicleSpecPattern as p
			inner join tbl_tmpPatterns as ptrn on p.VSpecSchemaPatternId = ptrn.id 
			left outer join DecodingItems as d on (d.DecodingItem)."DecodingId" = pass and p.ElementId = (d.DecodingItem)."ElementId" and LOWER(p.AttributeId) = LOWER((d.DecodingItem)."AttributeId")
		where 
			p.IsKey = true
		group by p.VSpecSchemaPatternId
		having count(*) <> count(distinct d.Id);

		delete from tbl_tmpPatterns where id in (select id from tbl_tmpPatternsEx); 

		create temporary table IF NOT EXISTS tbl_tbl1 (
			IsKey boolean, 
			vSpecSchemaId int, 
			vSpecPatternId int, 
			ElementId int, 
			AttributeId varchar(500), 
			ChangedOn timestamp null,
			TobeQCed boolean null
		) on commit drop;
		
		INSERT INTO tbl_tbl1 (iskey, vSpecSchemaId, vSpecPatternId, ElementId, AttributeId, ChangedOn, TobeQCed)
	    SELECT DISTINCT
	        vsp.IsKey,
	        vsvp.SchemaId,
	        vsp.vspecschemapatternid,
	        vsp.ElementId,
	        vsp.AttributeId,
	        COALESCE(vsp.UpdatedOn, vsp.CreatedOn),
	        ptrn.TobeQCed
	    FROM vpic.VehicleSpecPattern as vsp
	    INNER JOIN vpic.VSpecSchemaPattern as vsvp ON vsvp.id = vsp.vspecschemapatternid
	    INNER JOIN tbl_tmpPatterns as ptrn ON vsvp.id = ptrn.id
	    WHERE
	        vsp.IsKey = FALSE
	        AND vsp.ElementId NOT IN (
	            SELECT (di.DecodingItem)."ElementId"
	            FROM DecodingItems di
	            WHERE (di.DecodingItem)."DecodingId" = pass
	            AND (di.DecodingItem)."ElementId" NOT IN (1, 114, 121, 129, 150, 154, 155, 169, 186)
	        );

		DELETE FROM tbl_tbl1
		WHERE ctid IN (
		    SELECT ctid
		    FROM (
		        SELECT
		            ctid,
		            ROW_NUMBER() OVER(PARTITION BY elementid ORDER BY ChangedOn desc) AS rn
		        FROM tbl_tbl1
		    ) AS cte
		    WHERE rn > 1
		);

		insert into DecodingItems (DecodingItem) select distinct ROW(
			null, pass, t1.ChangedOn, t1.vSpecPatternId, '', t1.vSpecSchemaId,
			null, t1.ElementId, t1.AttributeId, 'XXX', 'Vehicle Specs', -100, t1.TobeQCed)::vpic."tblDecodingItem"
			FROM tbl_tbl1 as t1;

		if (select COUNT(*) from DecodingItems di where (di.DecodingItem)."DecodingId" = pass and not ((di.DecodingItem)."PatternId") is null) = 0 then
			ReturnCode = ReturnCode || ' 8 ';
			CorrectedVIN = '';
			ErrorBytes = '';
		else
			select err_returncode, err_correctedvin, err_errorbytes, err_unusedpositions 
			into ReturnCode, CorrectedVin, ErrorBytes, UnUsedPositions
			from vpic.spvindecode_errorcode(var_vin, modelYear);
		end if;

		drop table tbl_tmpPatterns;
		drop table tbl_tmpPatternsEx;
		drop table tbl_tbl1;
	end if;

	if exists(select 1 from DecodingItems as di where (di.DecodingItem)."DecodingId" = pass and (di.DecodingItem)."ElementId" = 5 and (di.DecodingItem)."AttributeId" = 64::varchar) then
		ReturnCode = ReturnCode || ' 9 ';
	end if;

	if exists(select 1 from DecodingItems as di where (di.DecodingItem)."DecodingId" = pass and (di.DecodingItem)."ElementId" = 5 and (di.DecodingItem)."AttributeId" in (69::varchar, 84::varchar, 86::varchar, 88::varchar, 97::varchar, 105::varchar, 113::varchar, 124::varchar, 126::varchar, 127::varchar)) then
		ReturnCode = ReturnCode || ' 10 ';
		isOffRoad = true;
	end if;

	if modelYear is null then
		ReturnCode = ReturnCode || ' 11 ';
	end if;

	select (di.DecodingItem)."AttributeId" into vehicleType from DecodingItems as di where (di.DecodingItem)."DecodingId" = pass and (di.DecodingItem)."ElementId" = 39;

	if exists(select 1 from vpic.VinException as v where v."vin" = var_vin and CheckDigit = true) then
		isVinExceptionCheckDigit = true;
	end if;

	if SUBSTRING(var_vin, 3, 1) = '9' then
		startPos = 15;
	else
		select vehicleTypeId, truckTypeId into x_vehicleTypeId, x_truckTypeId from vpic.Wmi where wmi = var_wmi;
		if x_vehicleTypeId in (2, 7) or (x_vehicleTypeId = 3 and x_truckTypeId = 1) then
			startPos = 13;
			isCarMpvLt = true;
		else
			startPos = 14;
		end if;
	end if;

	while j < length(var_vin) loop
		j = j + 1;
		if j = 9 and (isOffRoad = true or isVinExceptionCheckDigit = true) then
			continue;
		end if;

		chr = substring(var_vin, j, 1);
		if j <> 9 and j < startPos and chr !~ '^[0-9ABCDEFGHJKLMNPRSTUVWXYZ*]$'
				or j <> 9 and j >= startPos and chr !~ '^[0-9*]$'
				or j = 9 and chr !~ '^[0-9X*]$'
				or j = 10 and chr !~ '^[1-9ABCDEFGHJKLMNPRSTVWXY]$' then
			if chr = '' then
				chr = '_';
			end if;
			if CorrectedVIN = '' then
				CorrectedVIN = var_vin;
			end if;

			invalidChars = invalidChars || ', ' || cast(j as varchar(2)) || ':' || chr;
			correctedVIN = left(correctedVIN, j-1) || '!' || substring(correctedVIN, j+1, 100);
		end if;
	end loop;

	if invalidChars <> '' then
		ReturnCode = ReturnCode || ' 400 ';
	end if;

	if coalesce(Error12, false) = true then
		ReturnCode = ReturnCode || ' 12 ';
	end if;

	insert into DecodingItems (DecodingItem) select distinct ROW(
			null,
			pass,
			coalesce(dv.UpdatedOn, dv.CreatedOn),
			null,
			null,
			null,
			null,
			dv.ElementId,
			dv.DefaultValue,
			case when e.datatype = 'lookup' and dv.DefaultValue = '0' then 'Not Applicable' else 'XXX' end,
			'Default',
			10,
			null)::vpic."tblDecodingItem"
			FROM
				vpic.DefaultValue dv
				INNER JOIN vpic.Element e ON dv.ElementId = e.Id
			WHERE
				dv.VehicleTypeId = cast(vehicleType as integer) and dv.DefaultValue is not null and dv.elementid not in (select distinct (di.DecodingItem)."ElementId" from DecodingItems di where (di.DecodingItem)."DecodingId" = pass);

	if length(var_vin) < 17 then
		ReturnCode = ReturnCode || ' 6 ';
	else
		CD = substring(var_vin, 9, 1);
		calcCD = vpic.fVINCheckDigit2(var_vin, isCarmpvLT);
		if (cd <> calcCD) and (isVinExceptionCheckDigit = false) then
			ReturnCode = ReturnCode || ' 1 ';
		end if;
	end if;

	errors = ReturnCode;
	errors = replace(errors, ' 9 ', '');
	errors = replace(errors, ' 10 ', '');
	errors = replace(errors, ' 12 ', '');
	errors = trim(errors);

	if errors = '' or errors = '14' then
		ReturnCode = ' 0 ' || ReturnCode;
	end if;

	select count(*) into cnt from DecodingItems as di where (di.DecodingItem)."ElementId" = 28;

	if ReturnCode like '% 0 %' and cnt = 0 then
		ReturnCode = ReturnCode || ' 14 ';
	end if;

	if ReturnCode like '% 4 %' then
		select coalesce(additionalerrortext, '') into AdditionalDecodingInfo from vpic.ErrorCode where id = 4;
	end if;
	if ReturnCode like '% 5 %' then
		select coalesce(additionalerrortext, '') into AdditionalDecodingInfo from vpic.ErrorCode where id = 5;
	end if;
	if ReturnCode like '% 14 %' then
		AdditionalDecodingInfo = SUBSTRING(trim(coalesce(AdditionalDecodingInfo, '') || ' Unused position(s): ' || UnUsedPositions || '. ') from 1 for 500);
	end if;
	if ReturnCode like '% 400 %' then
		AdditionalDecodingInfo = SUBSTRING(trim(coalesce(AdditionalDecodingInfo, '') || ' Invalid character(s): ' || SUBSTRING(invalidChars, 3, LENGTH(invalidChars) - 2) || '. ') from 1 for 500);
	end if;

	if vehicleType = cast(10 as varchar) or exists(select 1 from DecodingItems di where (di.DecodingItem)."ElementId" = 5 and (di.DecodingItem)."AttributeId" in (65::varchar, 107::varchar, 70::varchar, 74::varchar, 63::varchar, 72::varchar, 112::varchar, 62::varchar, 64::varchar, 76::varchar, 78::varchar, 71::varchar, 77::varchar, 67::varchar, 116::varchar, 75::varchar) and (di.DecodingItem)."DecodingId" = pass) then
		AdditionalDecodingInfo = SUBSTRING(trim(coalesce(AdditionalDecodingInfo, '') || ' Incomplete Vehicle Warning - Please be advised that the vehicle may have been altered and may not be an accurate representation of the vehicle in its current condition. ') from 1 for 500);
	end if;

	if conclusive = false then
		AdditionalDecodingInfo = SUBSTRING(trim(coalesce(AdditionalDecodingInfo, '') || ' The Model Year decoded for this VIN may be incorrect. If you know the Model year, please enter it and decode again to get more accurate information. ') from 1 for 500);
	end if;

	SELECT 
        string_agg(trim(name), '; '),
        string_agg(id::TEXT, ',')
    INTO errorMessages, errorCodes
    FROM (
        SELECT 
            id,
            Name ||
                CASE
                    WHEN isOffRoad = true AND id = 1 THEN offRoadNote
                    WHEN isVinExceptionCheckDigit = true AND id = 0 THEN checkDigitExclusionNote
                    ELSE ''
                END AS name
        FROM vpic.ErrorCode
        WHERE ReturnCode LIKE '% ' || id::TEXT || ' %'
        ORDER BY id
    ) AS t;

	  errorMessages = left(errorMessages, 500);

	  insert into DecodingItems (DecodingItem) select distinct ROW(
			null, pass, null, null, '', null, null, p.ElementId, p.AttributeId, p.Value, 'Corrections', 999, null)::vpic."tblDecodingItem"
		from (
			select 142 as ElementId, CorrectedVIN as AttributeId, CorrectedVIN as Value
			union 
			select 143, errorCodes, errorCodes 
			union 
			select 191, errorMessages, errorMessages 
			union 
			select 144, ErrorBytes, ErrorBytes
			union 
			select 156, AdditionalDecodingInfo, AdditionalDecodingInfo
			union 
			select 196, descriptor, descriptor 
		) as p; 

	return query select (di.DecodingItem)."DecodingId" as CoreDecodingId, (di.DecodingItem)."CreatedOn" as CoreCreatedOn,
	(di.DecodingItem)."PatternId" as CorePatternId, (di.DecodingItem)."Keys" as COreKeys, (di.DecodingItem)."VinSchemaId" as CoreVinSchemaId,
	(di.DecodingItem)."WmiId" as CoreWmiId, (di.DecodingItem)."ElementId" as CoreElementId, (di.DecodingItem)."AttributeId" as CoreAttributeId,
	(di.DecodingItem)."Value" as CoreValue, (di.DecodingItem)."Source" as CoreSource, (di.DecodingItem)."Priority" as CorePriority, (di.DecodingItem)."TobeQCed" as CoreTobeQCed, ReturnCode as CoreReturnCode from DecodingItems di;
	
	drop table DecodingItems;
end;
$_$;
