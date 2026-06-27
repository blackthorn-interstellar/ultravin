CREATE FUNCTION vpic.spvindecode(v character varying, includeprivate boolean DEFAULT false, year integer DEFAULT NULL::integer, includeall boolean DEFAULT NULL::boolean, nooutput boolean DEFAULT false) RETURNS TABLE(groupname character varying, variable character varying, value character varying, itempatternid integer, itemvinschemaid integer, itemkeys character varying, itemelementid integer, itemattributeid character varying, itemcreatedon timestamp without time zone, itemwmiid integer, code character varying, datatype character varying, decode character varying, itemsource character varying, itemtobeqced boolean)
    LANGUAGE plpgsql
    AS $$
declare
	make varchar(50) = '';
	includeNotPublicilyAvailable boolean = null;
	vin varchar(17) = '';
	modelYear integer;
	modelYearSource varchar(20) = '***X*|Y';
	conclusive boolean = false;
	e12 boolean = false;
	ReturnCode varchar(100) = '';
	var_descriptor varchar(17);
	dmy integer = null;
	rmy integer;
	omy integer;
	do3and4 boolean = true;
	bestPass integer = 0;
	passes integer;
	v_limit integer;
	altMY integer = null;
	cnt1 integer = 0;
	cnt2 integer = 0;
begin

    v_limit = EXTRACT(YEAR FROM (CURRENT_DATE + INTERVAL '2 years'))::int;

	create temporary table IF NOT EXISTS DecItem (
	        ItemDecodingId integer, ItemCreatedOn timestamp without time zone, ItemPatternId integer,
			ItemKeys character varying(50), ItemVinSchemaId integer, ItemWmiId integer, ItemElementId integer,
			ItemAttributeId character varying(500), ItemValue character varying(500), ItemSource character varying(50), 
			ItemPriority integer, ItemTobeQCed boolean, ReturnCode varchar(100)
	    ) on commit drop;

	var_descriptor = vpic.fVinDescriptor(vin);
	vin = upper(trim(v));
	select vd.ModelYear into dmy from vpic.VinDescriptor vd where vd.Descriptor = var_descriptor;

	if dmy between 1980 and v_limit then
		conclusive = true;
		e12 = 
			CASE
				WHEN year IS NOT NULL and dmy IS NOT NULL and year <> dmy
				THEN true
		        ELSE false
			END;

		insert into DecItem (ItemDecodingId, ItemCreatedOn, ItemPatternId, ItemKeys, ItemVinSchemaId, ItemWmiId, ItemElementId, ItemAttributeId, ItemValue, ItemSource, ItemPriority, ItemTobeQCed, ReturnCode)
		select CoreDecodingId, CoreCreatedOn, CorePatternId, CoreKeys, CoreVinSchemaId, CoreWmiId, CoreElementId, CoreAttributeId, CoreValue, CoreSource, CorePriority, CoreTobeQCed, CoreReturnCode
		from vpic.spvindecode_core(1, dmy, vin, descriptor, conclusive, e12, includeAll, includePrivate, includeNotPublicilyAvailable);

		select di.ReturnCode into ReturnCode from DecItem di order by ItemDecodingId desc limit 1;
	else
		rmy = vpic.fVinModelYear2(upper(vin));
		conclusive = true;
		if rmy < 0 then
			omy = -rmy-30;
			rmy = -rmy;
			conclusive = false;
		end if;

		if conclusive = true then
			if rmy >= 1980 and rmy <= v_limit - 30 then
				altMY = rmy + 30;
			elsif rmy >= 1980 + 30 and rmy <= v_limit then
				altMY = rmy - 30;
			end if;

			if coalesce(altMY, rmy) <> rmy then
				cnt1 = 0;
				cnt2 = 0;
				
				select count(vs.Id) into cnt1
				from vpic.VinSchema vs
				inner join vpic.Wmi_VinSchema as wvs on vs.Id = wvs.VinSchemaId 
				inner join vpic.Wmi as w on wvs.WmiId = w.Id 
				where w.Wmi = vpic.fVinWMI(vin) and (rmy between wvs.YearFrom and coalesce(wvs.YearTo, 2999));

				select count(vs.Id) into cnt2
				from vpic.VinSchema vs
				inner join vpic.Wmi_VinSchema as wvs on vs.Id = wvs.VinSchemaId 
				inner join vpic.Wmi as w on wvs.WmiId = w.Id 
				where w.Wmi = vpic.fVinWMI(vin) and (altMY between wvs.YearFrom and coalesce(wvs.YearTo, 2999));

				if cnt1 = 0 and cnt2 > 0 then
					rmy = altMY;
				end if;
			end if;
		end if;

		if year between 1980 and v_limit then 
			if year = rmy or year = omy then
				do3and4 = true;
			else
				modelYearSource = cast(year as varchar);

				insert into DecItem (ItemDecodingId, ItemCreatedOn, ItemPatternId, ItemKeys, ItemVinSchemaId, ItemWmiId, ItemElementId, ItemAttributeId, ItemValue, ItemSource, ItemPriority, ItemTobeQCed, ReturnCode)
				select CoreDecodingId, CoreCreatedOn, CorePatternId, CoreKeys, CoreVinSchemaId, CoreWmiId, CoreElementId, CoreAttributeId, CoreValue, CoreSource, CorePriority, CoreTobeQCed, CoreReturnCode
				from vpic.spvindecode_core(2, year, vin, modelYearSource, true, true, includeAll, includePrivate, includeNotPublicilyAvailable) r;
		
				select di.ReturnCode into ReturnCode from DecItem di order by ItemDecodingId desc limit 1;
				
				do3and4 = 
					CASE
						WHEN ReturnCode LIKE '% 8 %' AND rmy IS NOT NULL
						THEN true
						ELSE false
			        END;
			end if;
		end if;

		if do3and4 = true then
			e12 = 
				CASE
					WHEN year IS NOT NULL and rmy IS NOT NULL and year <> rmy
					THEN true
			        ELSE false
				END;
			
			insert into DecItem (ItemDecodingId, ItemCreatedOn, ItemPatternId, ItemKeys, ItemVinSchemaId, ItemWmiId, ItemElementId, ItemAttributeId, ItemValue, ItemSource, ItemPriority, ItemTobeQCed, ReturnCode)
			select CoreDecodingId, CoreCreatedOn, CorePatternId, CoreKeys, CoreVinSchemaId, CoreWmiId, CoreElementId, CoreAttributeId, CoreValue, CoreSource, CorePriority, CoreTobeQCed, CoreReturnCode
			from vpic.spvindecode_core(3, rmy, vin, modelYearSource, conclusive, e12, includeAll, includePrivate, includeNotPublicilyAvailable);
			
			select di.ReturnCode into ReturnCode from DecItem di order by ItemDecodingId desc limit 1;
	
			if not omy is null then
				e12 = 
					CASE
						WHEN year IS NOT NULL and omy IS NOT NULL and year <> omy
						THEN true
				        ELSE false
					END;
				
				insert into DecItem (ItemDecodingId, ItemCreatedOn, ItemPatternId, ItemKeys, ItemVinSchemaId, ItemWmiId, ItemElementId, ItemAttributeId, ItemValue, ItemSource, ItemPriority, ItemTobeQCed, ReturnCode)
				select CoreDecodingId, CoreCreatedOn, CorePatternId, CoreKeys, CoreVinSchemaId, CoreWmiId, CoreElementId, CoreAttributeId, CoreValue, CoreSource, CorePriority, CoreTobeQCed, CoreReturnCode
				from vpic.spvindecode_core(4, omy, vin, modelYearSource, conclusive, e12, includeAll, includePrivate, includeNotPublicilyAvailable);

				select di.ReturnCode into ReturnCode from DecItem di order by ItemDecodingId desc limit 1;
			end if;
		end if;
	end if;

	select count(distinct ItemDecodingId) into passes from DecItem;

	create temporary table IF NOT EXISTS x (
	        ItemDecodingId integer, ErrorCodes varchar(100),
			ErrorValue integer, ElementsWeight integer,
			Patterns integer, ModelYear integer
	    ) on commit drop;

	insert into x
	select err.ItemDecodingId, err.ErrorCodes, err.ErrorValue, el.ElementsWeight, p.Patterns, my.ModelYear + my.ModelYearBonus as ModelYear
	from
	(
		select distinct ItemDecodingId
		from DecItem
	) a
	left outer join
	(	
		select d.ItemDecodingId, d.ItemValue as ErrorCodes, vpic.fErrorValue(d.ItemValue) as ErrorValue
		from DecItem d
		where d.ItemElementId = 143
	) err on a.ItemDecodingId = err.ItemDecodingId
	left outer join
	(	
		select ItemDecodingId, sum(weight) as ElementsWeight
		from (
			select distinct ItemDecodingId, d.ItemElementId, e.weight
			from DecItem d inner join vpic.Element e on d.ItemElementId = e.id 
			where coalesce(d.ItemValue, '') <> '' and e.weight is not null
		) t
		group by ItemDecodingId
	) el on err.ItemDecodingId = el.ItemDecodingId
	left outer join
	(	
		select ItemDecodingId, count(*) as Patterns
		from DecItem d 
		where d.ItemSource in ('Pattern', 'EngineModelPattern', 'Formula Pattern') and coalesce(d.ItemValue, '') not in ('', 'Not Applicable')
		group by ItemDecodingId
	) p on err.ItemDecodingId = p.ItemDecodingId
	left outer join
	(	
		select ItemDecodingId, cast(ItemValue as int) as ModelYear, case when year = cast(ItemValue as int) then 10000 else 0 end as ModelYearBonus
		from DecItem d
		where d.ItemElementId = 29
	) my on a.ItemDecodingId = my.ItemDecodingId;

	select ItemDecodingId into bestPass from x order by x.ErrorValue desc, x.ElementsWeight desc, x.Patterns desc, x.ModelYear desc limit 1;
		
	delete from DecItem where ItemDecodingId <> bestPass;

	update DecItem 
	set ItemTobeQCed = vs.TobeQCed
	from DecItem d inner join vpic.VinSchema vs on d.ItemVinSchemaId = vs.Id and vs.TobeQCed = true
	where lower(left(coalesce(d.ItemSource, ''), 7)) in ('pattern', 'formula', 'enginem', 'convers');

	if coalesce(includeNotPublicilyAvailable, false) = false then
		delete 
		from DecItem d
		where d.ItemTobeQCed = true;
	end if;

	update DecItem as t
	set ItemValue = case e.LookupTable when null then t.ItemAttributeId else (vpic.fElementAttributeValue (t.ItemElementId, t.ItemAttributeId)) end
	from vpic.Element e
	where t.ItemElementId = e.Id and t.ItemValue = 'XXX';

	if NoOutput = false then
		return query select 
			e.GroupName, 
			e.Name as Variable, 
			cast(REPLACE(REPLACE(REPLACE(t.ItemValue, CHR(9), ' '), CHR(13), ' '), CHR(10), ' ') as varchar) as Value, 
			t.ItemPatternId, 
			t.ItemVinSchemaId, 
			t.ItemKeys, 
			e.id as ItemElementId, 
			t.ItemAttributeId, 
			t.ItemCreatedOn as ItemCreatedOn, 
			t.ItemWmiId,
			e.Code, 
			e.DataType, 
			e.Decode,
			t.ItemSource, 
			t.ItemToBeQCed as ToBeQCd
		from 
			vpic.Element e
			left outer join DecItem t on t.ItemElementId = e.Id
		where 
			(coalesce(e.Decode, '') <> '') 
			and ((includeAll) = true or (coalesce(includeAll, false) = false and not t.ItemElementId is null)) 
			and (includePrivate = true or coalesce(e.IsPrivate, false) = false )
		order by
			CASE coalesce(e.GroupName, '')
			    WHEN '' THEN 0
			    WHEN 'General' THEN 1
				WHEN 'Exterior / Body' THEN 2
				WHEN 'Exterior / Dimension' THEN 3
				WHEN 'Exterior / Truck' THEN 4
				WHEN 'Exterior / Trailer' THEN 5
				WHEN 'Exterior / Wheel tire' THEN 6
				WHEN 'Exterior / Motorcycle' THEN 7
				WHEN 'Exterior / Bus' THEN 8
				WHEN 'Interior' THEN 9
				WHEN 'Interior / Seat' THEN 10
				WHEN 'Mechanical / Transmission' THEN 11
				WHEN 'Mechanical / Drivetrain' THEN 12
				WHEN 'Mechanical / Brake' THEN 13
				WHEN 'Mechanical / Battery' THEN 14
				WHEN 'Mechanical / Battery / Charger' THEN 15
				WHEN 'Engine' THEN 16
				WHEN 'Passive Safety System' THEN 17
				WHEN 'Passive Safety System / Air Bag Location' THEN 18
				WHEN 'Active Safety System' THEN 19
				WHEN 'Active Safety System / Maintaining Safe Distance' THEN 20
				WHEN 'Active Safety System / Forward Collision Prevention' THEN 21
				WHEN 'Active Safety System / Lane and Side Assist' THEN 22
				WHEN 'Active Safety System / Backing Up and Parking' THEN 23
				WHEN 'Active Safety System / 911 Notification' THEN 24
				WHEN 'Active Safety System / Lighting Technologies' THEN 25
				WHEN 'Internal' THEN 26
			    ELSE 99
			END;
	else
		-- insert into DecodingOutput (GroupName, Variable, Value, PatternId, VinSchemaId, Keys, ElementId, AttributeId, CreatedOn, WmiId, Code, DataType, Decode, Source)
		-- select 
		-- 	e.GroupName, 
		-- 	e.Name as Variable, 
		-- 	REPLACE(REPLACE(REPLACE(t.ItemValue, CHR(9), ' '), CHR(13), ' '), CHR(10), ' ') as Value, 
		-- 	t.ItemPatternId, 
		-- 	t.ItemVinSchemaId, 
		-- 	t.ItemKeys, 
		-- 	e.id as ElementId, 
		-- 	t.ItemAttributeId, 
		-- 	t.ItemCreatedOn as CreatedOn, 
		-- 	t.ItemWmiId,
		-- 	e.Code, 
		-- 	e.DataType, 
		-- 	e.Decode,
		-- 	t.ItemSource 
		-- from 
		-- 	vpic.Element e
		-- 	left outer join DecItem t on t.ItemElementId = e.Id
		-- where 
		-- 	(coalesce(e.Decode, '') <> '') 
		-- 	and ((includeAll) = true or (coalesce(includeAll, false) = false and not t.ItemElementId is null)) 
		-- 	and (includePrivate = true or coalesce(e.IsPrivate, false) = false )
		-- order by
		-- 	CASE coalesce(e.GroupName, '')
		-- 	    WHEN '' THEN 0
		-- 	    WHEN 'General' THEN 1
		-- 		WHEN 'Exterior / Body' THEN 2
		-- 		WHEN 'Exterior / Dimension' THEN 3
		-- 		WHEN 'Exterior / Truck' THEN 4
		-- 		WHEN 'Exterior / Trailer' THEN 5
		-- 		WHEN 'Exterior / Wheel tire' THEN 6
		-- 		WHEN 'Exterior / Motorcycle' THEN 7
		-- 		WHEN 'Exterior / Bus' THEN 8
		-- 		WHEN 'Interior' THEN 9
		-- 		WHEN 'Interior / Seat' THEN 10
		-- 		WHEN 'Mechanical / Transmission' THEN 11
		-- 		WHEN 'Mechanical / Drivetrain' THEN 12
		-- 		WHEN 'Mechanical / Brake' THEN 13
		-- 		WHEN 'Mechanical / Battery' THEN 14
		-- 		WHEN 'Mechanical / Battery / Charger' THEN 15
		-- 		WHEN 'Engine' THEN 16
		-- 		WHEN 'Passive Safety System' THEN 17
		-- 		WHEN 'Passive Safety System / Air Bag Location' THEN 18
		-- 		WHEN 'Active Safety System' THEN 19
		-- 		WHEN 'Active Safety System / Maintaining Safe Distance' THEN 20
		-- 		WHEN 'Active Safety System / Forward Collision Prevention' THEN 21
		-- 		WHEN 'Active Safety System / Lane and Side Assist' THEN 22
		-- 		WHEN 'Active Safety System / Backing Up and Parking' THEN 23
		-- 		WHEN 'Active Safety System / 911 Notification' THEN 24
		-- 		WHEN 'Active Safety System / Lighting Technologies' THEN 25
		-- 		WHEN 'Internal' THEN 26
		-- 	    ELSE 99
		-- 	END, e.id;
	end if;
		
	drop table DecItem;
	drop table x;
end;
$$;
